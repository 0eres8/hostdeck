//! Built-in RDP client. Each session runs IronRDP on its own thread and shares the remote
//! desktop image, connection status and clipboard text with the UI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use eframe::egui;
use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend};
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp::cliprdr::{Cliprdr, CliprdrClient};
use ironrdp::connector::connection_activation::ConnectionActivationState;
use ironrdp::connector::sspi;
use ironrdp::connector::sspi::credssp::NStatusCode;
use ironrdp::connector::{ClientConnector, ConnectorError, ConnectorErrorKind, Credentials, DesktopSize, ServerName};
use ironrdp::core::WriteBuf;
use ironrdp::displaycontrol::client::DisplayControlClient;
use ironrdp::displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp::dvc::DrdynvcClient;
use ironrdp::graphics::image_processing::PixelFormat;
use ironrdp::pdu::geometry::InclusiveRectangle;
use ironrdp::pdu::input::fast_path::FastPathInputEvent;
use ironrdp::pdu::nego::FailureCode;
use ironrdp::session::image::DecodedImage;
use ironrdp::session::{fast_path, ActiveStage, ActiveStageBuilder, ActiveStageOutput, GracefulDisconnectReason};
use ironrdp_tokio::reqwest::ReqwestNetworkClient;
use ironrdp_tokio::FramedWrite as _;
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::model::CertPolicy;

/// Everything needed to open one connection.
pub struct Params {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    pub password: String,
    pub clipboard: bool,
    pub cert_policy: CertPolicy,
    pub known_hosts: PathBuf,
}

/// Requests from the UI to the session thread.
pub enum Input {
    /// The size of the view in physical pixels, and the display scale in percent.
    Resize { width: u16, height: u16, scale: u32 },
    Events(Vec<FastPathInputEvent>),
    /// Text on the local clipboard, offered to the remote desktop when it changed.
    LocalClipboard(String),
    Clipboard(ClipboardMessage),
    Disconnect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct End {
    /// The session ended normally (logoff, disconnect) rather than failing.
    pub normal: bool,
    pub headline: String,
    pub detail: Option<String>,
    /// Fingerprint of a server certificate the user can choose to trust.
    pub untrusted_cert: Option<String>,
}

impl End {
    fn normal(headline: &str, detail: Option<&str>) -> Self {
        Self { normal: true, headline: headline.into(), detail: detail.map(Into::into), untrusted_cert: None }
    }

    fn failed(detail: impl Into<String>) -> Self {
        Self { normal: false, headline: "Connection failed".into(), detail: Some(detail.into()), untrusted_cert: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Connecting,
    Connected,
    Ended(End),
}

/// The remote desktop image as RGBA pixels, with the area changed since the UI last looked.
#[derive(Default)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    /// Changed area as `[x, y, width, height]`.
    pub dirty: Option<[usize; 4]>,
}

pub struct Shared {
    pub phase: Phase,
    pub frame: Frame,
    pub log: Vec<String>,
    /// Text the remote desktop copied, waiting to be put on the local clipboard.
    pub remote_clipboard: Option<String>,
}

impl Shared {
    fn log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > 300 {
            self.log.drain(..50);
        }
    }
}

pub struct RdpSession {
    tx: mpsc::UnboundedSender<Input>,
    shared: Arc<Mutex<Shared>>,
}

impl RdpSession {
    pub fn spawn(params: Params, ctx: egui::Context) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Arc::new(Mutex::new(Shared {
            phase: Phase::Connecting,
            frame: Frame::default(),
            log: Vec::new(),
            remote_clipboard: None,
        }));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let (thread_tx, thread_shared) = (tx.clone(), shared.clone());
        thread::Builder::new().name("rdp".into()).spawn(move || {
            let end = runtime.block_on(run(&params, rx, thread_tx, &thread_shared, &ctx));
            let mut s = thread_shared.lock().unwrap();
            if let Some(detail) = &end.detail {
                s.log(format!("{}: {detail}", end.headline));
            }
            s.phase = Phase::Ended(end);
            drop(s);
            ctx.request_repaint();
        })?;
        Ok(Self { tx, shared })
    }

    pub fn send(&self, input: Input) {
        let _ = self.tx.send(input);
    }

    pub fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap()
    }

    pub fn is_running(&self) -> bool {
        !matches!(self.shared().phase, Phase::Ended(_))
    }
}

impl Drop for RdpSession {
    fn drop(&mut self) {
        self.send(Input::Disconnect);
    }
}

fn log(shared: &Mutex<Shared>, line: impl Into<String>) {
    shared.lock().unwrap().log(line);
}

/// Split `DOMAIN\user` so the domain typed with the username wins over the domain field.
pub fn split_domain<'a>(username: &'a str, domain: &'a str) -> (&'a str, &'a str) {
    match username.split_once('\\') {
        Some((d, u)) => (u, d),
        None => (username, domain),
    }
}

async fn run(
    p: &Params,
    mut rx: mpsc::UnboundedReceiver<Input>,
    tx: mpsc::UnboundedSender<Input>,
    shared: &Arc<Mutex<Shared>>,
    ctx: &egui::Context,
) -> End {
    // Connect at the size of the view, which the UI reports on its first frame.
    let mut size = (1600, 900, 100);
    let mut local_clipboard = None;
    let wait = tokio::time::sleep(Duration::from_secs(3));
    tokio::pin!(wait);
    loop {
        tokio::select! {
            input = rx.recv() => match input {
                Some(Input::Resize { width, height, scale }) => {
                    size = (width, height, scale);
                    break;
                }
                Some(Input::LocalClipboard(text)) => local_clipboard = Some(text),
                Some(Input::Disconnect) | None => return End::normal("Disconnected", None),
                Some(_) => {}
            },
            _ = &mut wait => break,
        }
    }

    let (width, height) = MonitorLayoutEntry::adjust_display_size(u32::from(size.0), u32::from(size.1));
    log(shared, format!("Connecting to {}:{} at {width}×{height}", p.host, p.port));

    let addrs: Vec<_> = match tokio::net::lookup_host((p.host.as_str(), p.port)).await {
        Ok(a) => a.collect(),
        Err(_) => return End::failed("The host name could not be resolved."),
    };
    let stream = match tokio::time::timeout(Duration::from_secs(15), TcpStream::connect(&addrs[..])).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            log(shared, format!("TCP connect: {e}"));
            return End::failed("Could not reach the server. Check the host, port and firewall.");
        }
        Err(_) => return End::failed("The server did not answer. Check the host, port and firewall."),
    };
    let client_addr = match stream.local_addr() {
        Ok(a) => a,
        Err(e) => return End::failed(e.to_string()),
    };

    let mut connector = ClientConnector::new(connector_config(p, width as u16, height as u16, size.2), client_addr)
        .with_static_channel(DrdynvcClient::new().with_dynamic_channel(DisplayControlClient::new(|_| Ok(Vec::new()))));
    if p.clipboard {
        let backend = ClipboardBackend {
            tx: tx.clone(),
            local: local_clipboard,
            last_synced: None,
            shared: shared.clone(),
            ctx: ctx.clone(),
        };
        connector.attach_static_channel(Cliprdr::new(Box::new(backend)));
    }

    let mut framed = ironrdp_tokio::TokioFramed::new(stream);
    let should_upgrade = match ironrdp_tokio::connect_begin(&mut framed, &mut connector).await {
        Ok(u) => u,
        Err(e) => return connect_failure(shared, &e),
    };
    let (stream, leftover) = framed.into_inner();
    let (tls, cert) = match ironrdp_tls::upgrade(stream, &p.host).await {
        Ok(t) => t,
        Err(e) => {
            log(shared, format!("TLS: {e}"));
            return End::failed("The TLS handshake with the server failed.");
        }
    };

    // Check the certificate before any credentials are sent.
    let der = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| c.to_vec()).unwrap_or_default();
    let fp = fingerprint(&der);
    if let Err(end) = check_certificate(p, &fp, shared) {
        return end;
    }

    let upgraded = ironrdp_tokio::mark_as_upgraded(should_upgrade, &mut connector);
    let Some(server_public_key) = ironrdp_tls::extract_tls_server_public_key(&cert).map(<[u8]>::to_vec) else {
        return End::failed("The server certificate has no usable public key.");
    };
    let mut framed = ironrdp_tokio::TokioFramed::new_with_leftover(tls, leftover);
    let result = ironrdp_tokio::connect_finalize(
        upgraded,
        connector,
        &mut framed,
        &mut ReqwestNetworkClient::new(),
        ServerName::new(&p.host),
        server_public_key,
        None,
    )
    .await;
    let result = match result {
        Ok(r) => r,
        Err(e) => return connect_failure(shared, &e),
    };

    {
        let mut s = shared.lock().unwrap();
        s.log(format!("Connected, desktop {}×{}", result.desktop_size.width, result.desktop_size.height));
        s.phase = Phase::Connected;
    }
    ctx.request_repaint();

    match active_session(framed, result, &mut rx, shared, ctx).await {
        Ok(end) => end,
        Err(e) => {
            log(shared, format!("Session: {}", e.report()));
            End { normal: false, headline: "Connection lost".into(), detail: Some(e.to_string()), untrusted_cert: None }
        }
    }
}

fn connector_config(p: &Params, width: u16, height: u16, scale: u32) -> ironrdp::connector::Config {
    use ironrdp::pdu::gcc::KeyboardType;
    use ironrdp::pdu::rdp::capability_sets::{client_codecs_capabilities, MajorPlatformType};
    use ironrdp::pdu::rdp::client_info::{CompressionType, PerformanceFlags, TimezoneInfo};

    let (username, domain) = split_domain(&p.username, &p.domain);
    let client_name: String = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_else(|_| "hostdeck".into())
        .trim()
        .chars()
        .take(15)
        .collect();
    ironrdp::connector::Config {
        desktop_size: DesktopSize { width, height },
        desktop_scale_factor: scale,
        enable_tls: true,
        enable_credssp: true,
        credentials: Credentials::UsernamePassword { username: username.into(), password: p.password.clone() },
        domain: (!domain.is_empty()).then(|| domain.to_string()),
        client_build: 0,
        client_name,
        keyboard_type: KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        keyboard_layout: 0,
        ime_file_name: String::new(),
        bitmap: Some(ironrdp::connector::BitmapConfig {
            lossy_compression: true,
            color_depth: 32,
            codecs: client_codecs_capabilities(&[]).unwrap_or_default(),
        }),
        dig_product_id: String::new(),
        client_dir: String::new(),
        alternate_shell: String::new(),
        work_dir: String::new(),
        platform: MajorPlatformType::UNIX,
        hardware_id: None,
        request_data: None,
        autologon: true,
        enable_audio_playback: false,
        performance_flags: PerformanceFlags::default(),
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
        compression_type: Some(CompressionType::K64),
        // The remote pointer is drawn into the image, so it always matches the remote shape.
        enable_server_pointer: true,
        pointer_software_rendering: true,
        multitransport_flags: None,
    }
}

/// A human explanation of a failed connection attempt.
fn connect_failure(shared: &Mutex<Shared>, e: &ConnectorError) -> End {
    log(shared, format!("Connect: {}", e.report()));
    let detail = match e.kind() {
        ConnectorErrorKind::Credssp(se) => explain_credssp(se),
        ConnectorErrorKind::AccessDenied => {
            "The server denied access. The account may not be allowed to log on remotely; add it to the Remote Desktop Users group on the server.".into()
        }
        ConnectorErrorKind::Negotiation(f) => match f.code() {
            FailureCode::HYBRID_REQUIRED_BY_SERVER => "The server requires Network Level Authentication.".into(),
            FailureCode::SSL_REQUIRED_BY_SERVER | FailureCode::SSL_WITH_USER_AUTH_REQUIRED_BY_SERVER => {
                "The server refused the security settings of this connection.".into()
            }
            FailureCode::SSL_NOT_ALLOWED_BY_SERVER => {
                "The server only accepts the legacy RDP security protocol, which is not supported. Use the separate FreeRDP window for this server.".into()
            }
            _ => format!("Security negotiation failed: {f}"),
        },
        _ => e.to_string(),
    };
    End::failed(detail)
}

fn explain_credssp(e: &sspi::Error) -> String {
    let by_status = e.nstatus.and_then(|s| match s {
        NStatusCode::LOGON_FAILURE | NStatusCode::WRONG_PASSWORD | NStatusCode::NO_SUCH_USER => {
            Some("The server rejected the username or password.")
        }
        NStatusCode::ACCOUNT_LOCKED_OUT => Some("The account is locked out."),
        NStatusCode::ACCOUNT_DISABLED => Some("The account is disabled."),
        NStatusCode::PASSWORD_EXPIRED => Some("The password has expired. Change it on the server first."),
        NStatusCode::PASSWORD_MUST_CHANGE => Some("The password must be changed before logging on."),
        NStatusCode::ACCOUNT_RESTRICTION => {
            Some("An account restriction prevents logging on, for example a blank password.")
        }
        NStatusCode::LOGON_TYPE_NOT_GRANTED => {
            Some("This account may not log on remotely. Add it to the Remote Desktop Users group on the server.")
        }
        NStatusCode::NO_LOGON_SERVERS => Some("The server could not reach a domain controller to check the logon."),
        NStatusCode::TIME_DIFFERENCE_AT_DC => Some("The clocks of this computer and the domain differ too much."),
        _ => None,
    });
    if let Some(why) = by_status {
        return why.into();
    }
    match e.error_type {
        sspi::ErrorKind::LogonDenied | sspi::ErrorKind::UnknownCredentials => {
            "The server rejected the username or password.".into()
        }
        sspi::ErrorKind::NoCredentials => "No username or password was given.".into(),
        sspi::ErrorKind::NoAuthenticatingAuthority => {
            "The domain could not be contacted to check the logon. Check the domain name.".into()
        }
        sspi::ErrorKind::TimeSkew => "The clocks of this computer and the server differ too much.".into(),
        _ => format!("Network Level Authentication failed: {}", e.description),
    }
}

// ── Server certificates ──────────────────────────────────────────────────────

pub fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    let hex: Vec<String> = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("SHA256:{}", hex.join(""))
}

fn host_key(host: &str, port: u16) -> String {
    format!("{}:{port}", host.to_lowercase())
}

/// Trusted certificates as `host:port fingerprint` lines.
fn load_known_hosts(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(k, v)| (k.to_string(), v.trim().to_string()))
        .collect()
}

pub fn trust_certificate(path: &Path, host: &str, port: u16, fp: &str) -> Result<()> {
    let mut known = load_known_hosts(path);
    known.insert(host_key(host, port), fp.to_string());
    let text: String = known.iter().map(|(k, v)| format!("{k} {v}\n")).collect();
    crate::model::write_private(path, &text)
}

fn check_certificate(p: &Params, fp: &str, shared: &Mutex<Shared>) -> Result<(), End> {
    if p.cert_policy == CertPolicy::Ignore {
        return Ok(());
    }
    let untrusted = |headline: &str, detail: String| End {
        normal: false,
        headline: headline.into(),
        detail: Some(detail),
        untrusted_cert: Some(fp.to_string()),
    };
    match load_known_hosts(&p.known_hosts).get(&host_key(&p.host, p.port)) {
        Some(known) if known == fp => Ok(()),
        Some(known) => {
            log(shared, format!("Certificate changed: was {known}, now {fp}"));
            Err(untrusted(
                "Server certificate changed",
                format!("The certificate of this server is different from the last connection. This can mean the server was reinstalled, or that someone is intercepting the connection. Only trust it if you expect the change.\n\nNew fingerprint: {fp}"),
            ))
        }
        None if p.cert_policy == CertPolicy::Deny => Err(untrusted(
            "Server certificate not trusted",
            format!("This server's certificate has not been trusted yet. Check the fingerprint with the server administrator before trusting it.\n\nFingerprint: {fp}"),
        )),
        None => {
            log(shared, format!("Trusting the certificate on first use: {fp}"));
            trust_certificate(&p.known_hosts, &p.host, p.port, fp)
                .map_err(|e| End::failed(format!("Could not save the server certificate: {e:#}")))
        }
    }
}

// ── Clipboard ────────────────────────────────────────────────────────────────

/// Shares plain text with the remote clipboard. Remote copies are fetched right away and put
/// on the local clipboard; local text is offered when the UI reports a paste.
struct ClipboardBackend {
    tx: mpsc::UnboundedSender<Input>,
    local: Option<String>,
    /// The text both sides last agreed on, so an unchanged local clipboard never replaces
    /// something newer that was copied on the remote side.
    last_synced: Option<String>,
    shared: Arc<Mutex<Shared>>,
    ctx: egui::Context,
}

ironrdp::core::impl_as_any!(ClipboardBackend);

impl std::fmt::Debug for ClipboardBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClipboardBackend")
    }
}

impl ClipboardBackend {
    fn send(&self, message: ClipboardMessage) {
        let _ = self.tx.send(Input::Clipboard(message));
    }

    fn formats(&self) -> Vec<ClipboardFormat> {
        match self.local {
            Some(_) => vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)],
            None => Vec::new(),
        }
    }
}

impl CliprdrBackend for ClipboardBackend {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        self.last_synced = self.local.clone();
        self.send(ClipboardMessage::SendInitiateCopy(self.formats()));
    }

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        if available_formats.iter().any(|f| f.id() == ClipboardFormatId::CF_UNICODETEXT) {
            self.send(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::CF_UNICODETEXT));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let response = match &self.local {
            Some(text) if request.format == ClipboardFormatId::CF_UNICODETEXT => {
                FormatDataResponse::new_unicode_string(text)
            }
            _ => FormatDataResponse::new_error(),
        };
        self.send(ClipboardMessage::SendFormatData(response));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        if let Ok(text) = response.to_unicode_string() {
            let text = text.trim_end_matches('\0').replace("\r\n", "\n");
            self.local = Some(text.clone());
            self.last_synced = Some(text.clone());
            self.shared.lock().unwrap().remote_clipboard = Some(text);
            self.ctx.request_repaint();
        }
    }

    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _: LockDataId) {}

    fn on_unlock(&mut self, _: LockDataId) {}
}

// ── Active session ───────────────────────────────────────────────────────────

type Framed = ironrdp_tokio::TokioFramed<ironrdp_tls::TlsStream<TcpStream>>;

async fn active_session(
    framed: Framed,
    result: ironrdp::connector::ConnectionResult,
    rx: &mut mpsc::UnboundedReceiver<Input>,
    shared: &Arc<Mutex<Shared>>,
    ctx: &egui::Context,
) -> ironrdp::session::SessionResult<End> {
    use ironrdp::session::custom_err;

    let (mut reader, mut writer) = ironrdp_tokio::split_tokio_framed(framed);
    let size = result.desktop_size;
    let mut image = DecodedImage::new(PixelFormat::RgbA32, size.width, size.height);
    publish(shared, &image, None);
    let activation_factory = result.activation_factory;
    let mut stage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();

    // A resize asked for before the display control channel is ready is retried on the tick.
    let mut pending_resize: Option<(u32, u32, u32)> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    let reason = 'session: loop {
        // Input goes first, so typing and the pointer stay responsive while the server is
        // busy sending screen updates.
        let outputs = tokio::select! {
            biased;
            input = rx.recv() => match input {
                None | Some(Input::Disconnect) => break 'session None,
                Some(Input::Resize { width, height, scale }) => {
                    let (w, h) = MonitorLayoutEntry::adjust_display_size(u32::from(width), u32::from(height));
                    if (w, h) == (u32::from(image.width()), u32::from(image.height())) {
                        pending_resize = None;
                        Vec::new()
                    } else {
                        pending_resize = Some((w, h, scale));
                        resize_frame(&mut stage, &mut pending_resize)?
                    }
                }
                Some(Input::Events(events)) => stage.process_fastpath_input(&mut image, &events)?,
                Some(Input::LocalClipboard(text)) => offer_local_text(&mut stage, text)?,
                Some(Input::Clipboard(message)) => clipboard_frame(&mut stage, message)?,
            },
            _ = tick.tick() => resize_frame(&mut stage, &mut pending_resize)?,
            frame = reader.read_pdu() => {
                let (action, payload) = frame.map_err(|e| custom_err!("read frame", e))?;
                stage.process(&mut image, action, &payload)?
            }
        };

        let mut repaint = false;
        for out in outputs {
            match out {
                ActiveStageOutput::ResponseFrame(frame) => {
                    writer.write_all(&frame).await.map_err(|e| custom_err!("write response", e))?
                }
                ActiveStageOutput::GraphicsUpdate(region) => {
                    publish(shared, &image, Some(&region));
                    repaint = true;
                }
                ActiveStageOutput::DeactivateAll => {
                    // The server changed the desktop, e.g. after a resize: run the
                    // Deactivation-Reactivation Sequence and start over with a new image.
                    let mut activation = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = ironrdp_tokio::single_sequence_step_read(&mut reader, &mut activation, &mut buf)
                            .await
                            .map_err(|e| custom_err!("reactivation", e))?;
                        if written.size().is_some() {
                            writer.write_all(buf.filled()).await.map_err(|e| custom_err!("reactivation", e))?;
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                        } = activation.connection_activation_state()
                        {
                            log(shared, format!("Desktop resized to {}×{}", desktop_size.width, desktop_size.height));
                            image = DecodedImage::new(PixelFormat::RgbA32, desktop_size.width, desktop_size.height);
                            stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id: activation.io_channel_id(),
                                    user_channel_id: activation.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                    bulk_decompressor: None,
                                }
                                .build(),
                            );
                            stage.set_share_id(share_id);
                            stage.set_enable_server_pointer(enable_server_pointer);
                            publish(shared, &image, None);
                            repaint = true;
                            break;
                        }
                    }
                }
                ActiveStageOutput::Terminate(reason) => break 'session Some(reason),
                _ => {}
            }
        }
        if repaint {
            ctx.request_repaint();
        }
    };

    Ok(match reason {
        None => End::normal("Disconnected", None),
        Some(GracefulDisconnectReason::UserInitiated) => End::normal("Logged off", None),
        Some(GracefulDisconnectReason::ServerInitiated) => End::normal("Disconnected", Some("The server ended the session.")),
        Some(GracefulDisconnectReason::Other(why)) => End { detail: Some(why), ..End::normal("Disconnected", None) },
    })
}

fn resize_frame(
    stage: &mut ActiveStage,
    pending: &mut Option<(u32, u32, u32)>,
) -> ironrdp::session::SessionResult<Vec<ActiveStageOutput>> {
    let Some((width, height, scale)) = *pending else { return Ok(Vec::new()) };
    match stage.encode_resize(width, height, Some(scale), None) {
        Some(frame) => {
            *pending = None;
            Ok(vec![ActiveStageOutput::ResponseFrame(frame?)])
        }
        None => Ok(Vec::new()),
    }
}

fn offer_local_text(stage: &mut ActiveStage, text: String) -> ironrdp::session::SessionResult<Vec<ActiveStageOutput>> {
    use ironrdp::session::custom_err;
    let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else { return Ok(Vec::new()) };
    let Some(backend) = cliprdr.downcast_backend_mut::<ClipboardBackend>() else { return Ok(Vec::new()) };
    if backend.last_synced.as_ref() == Some(&text) {
        return Ok(Vec::new());
    }
    backend.local = Some(text.clone());
    backend.last_synced = Some(text);
    let messages = cliprdr
        .initiate_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)])
        .map_err(|e| custom_err!("CLIPRDR", e))?;
    Ok(vec![ActiveStageOutput::ResponseFrame(stage.process_svc_processor_messages(messages)?)])
}

fn clipboard_frame(
    stage: &mut ActiveStage,
    message: ClipboardMessage,
) -> ironrdp::session::SessionResult<Vec<ActiveStageOutput>> {
    use ironrdp::session::custom_err;
    let Some(cliprdr) = stage.get_svc_processor_mut::<CliprdrClient>() else { return Ok(Vec::new()) };
    let messages = match message {
        ClipboardMessage::SendInitiateCopy(formats) => cliprdr.initiate_copy(&formats),
        ClipboardMessage::SendFormatData(response) => cliprdr.submit_format_data(response),
        ClipboardMessage::SendInitiatePaste(format) => cliprdr.initiate_paste(format),
        _ => return Ok(Vec::new()),
    }
    .map_err(|e| custom_err!("CLIPRDR", e))?;
    Ok(vec![ActiveStageOutput::ResponseFrame(stage.process_svc_processor_messages(messages)?)])
}

/// Copy the changed part of the image (or all of it) into the frame the UI reads.
fn publish(shared: &Mutex<Shared>, image: &DecodedImage, region: Option<&InclusiveRectangle>) {
    let (w, h) = (usize::from(image.width()), usize::from(image.height()));
    if w == 0 || h == 0 {
        return;
    }
    let mut s = shared.lock().unwrap();
    let f = &mut s.frame;
    let full = [0, 0, w - 1, h - 1];
    let [x0, y0, x1, y1] = if f.width != w || f.height != h {
        f.width = w;
        f.height = h;
        f.pixels = vec![0; w * h * 4];
        f.dirty = None;
        full
    } else {
        match region {
            Some(r) => [usize::from(r.left), usize::from(r.top), usize::from(r.right).min(w - 1), usize::from(r.bottom).min(h - 1)],
            None => full,
        }
    };
    if x0 > x1 || y0 > y1 {
        return;
    }
    let data = image.data();
    for y in y0..=y1 {
        let (start, end) = ((y * w + x0) * 4, (y * w + x1 + 1) * 4);
        let row = &mut f.pixels[start..end];
        row.copy_from_slice(&data[start..end]);
        row.as_chunks_mut::<4>().0.iter_mut().for_each(|px| px[3] = 255);
    }
    f.dirty = Some(union(f.dirty, [x0, y0, x1 - x0 + 1, y1 - y0 + 1]));
}

fn union(a: Option<[usize; 4]>, b: [usize; 4]) -> [usize; 4] {
    let Some(a) = a else { return b };
    let (x0, y0) = (a[0].min(b[0]), a[1].min(b[1]));
    let (x1, y1) = ((a[0] + a[2]).max(b[0] + b[2]), (a[1] + a[3]).max(b[1] + b[3]));
    [x0, y0, x1 - x0, y1 - y0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_in_username_wins() {
        assert_eq!(split_domain("CORP\\administrator", ""), ("administrator", "CORP"));
        assert_eq!(split_domain("CORP\\administrator", "OTHER"), ("administrator", "CORP"));
        assert_eq!(split_domain("administrator", "CORP"), ("administrator", "CORP"));
        assert_eq!(split_domain("admin@corp.example", ""), ("admin@corp.example", ""));
    }

    #[test]
    fn dirty_areas_grow_to_cover_both() {
        assert_eq!(union(None, [5, 5, 10, 10]), [5, 5, 10, 10]);
        assert_eq!(union(Some([0, 0, 4, 4]), [10, 2, 5, 5]), [0, 0, 15, 7]);
    }

    #[test]
    fn known_hosts_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hostdeck-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("rdp_known_hosts");
        trust_certificate(&path, "Win01", 3389, "SHA256:aa").unwrap();
        trust_certificate(&path, "db", 3390, "SHA256:bb").unwrap();
        trust_certificate(&path, "win01", 3389, "SHA256:cc").unwrap();
        let known = load_known_hosts(&path);
        assert_eq!(known.get("win01:3389").map(String::as_str), Some("SHA256:cc"));
        assert_eq!(known.get("db:3390").map(String::as_str), Some("SHA256:bb"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// End-to-end against a real server, without the UI:
    /// `HOSTDECK_RDP_TEST=host:port:user:password cargo test -- --ignored live_server`
    #[test]
    #[ignore]
    fn live_server() {
        let target = std::env::var("HOSTDECK_RDP_TEST").expect("set HOSTDECK_RDP_TEST=host:port:user:password");
        let mut parts = target.splitn(4, ':');
        let (host, port, user, pass) = (
            parts.next().unwrap().to_string(),
            parts.next().unwrap().parse().unwrap(),
            parts.next().unwrap().to_string(),
            parts.next().unwrap().to_string(),
        );
        let dir = std::env::temp_dir().join(format!("hostdeck-test-{}", uuid::Uuid::new_v4()));
        let params = |password: &str| Params {
            host: host.clone(),
            port,
            username: user.clone(),
            domain: String::new(),
            password: password.into(),
            clipboard: true,
            cert_policy: CertPolicy::Tofu,
            known_hosts: dir.join("rdp_known_hosts"),
        };
        let wait_for = |s: &RdpSession, what: &str, ok: &dyn Fn(&Shared) -> bool| {
            let start = std::time::Instant::now();
            while !ok(&s.shared()) {
                assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {what}: {:?}", s.shared().log);
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let ctx = egui::Context::default();

        // A wrong password is reported as such.
        let s = RdpSession::spawn(params("wrong"), ctx.clone()).unwrap();
        s.send(Input::Resize { width: 1024, height: 768, scale: 100 });
        wait_for(&s, "the logon failure", &|sh| matches!(sh.phase, Phase::Ended(_)));
        let Phase::Ended(end) = s.shared().phase.clone() else { unreachable!() };
        println!("wrong password: {end:?}");
        assert!(!end.normal);
        assert_eq!(end.detail.as_deref(), Some("The server rejected the username or password."));

        // The right password connects, the first certificate is trusted, and frames arrive.
        let s = RdpSession::spawn(params(&pass), ctx.clone()).unwrap();
        s.send(Input::Resize { width: 1024, height: 768, scale: 100 });
        wait_for(&s, "the connection", &|sh| sh.phase == Phase::Connected);
        wait_for(&s, "a frame", &|sh| sh.frame.dirty.is_some() && sh.frame.pixels.iter().any(|&b| b != 0 && b != 255));
        let (w, h) = { let sh = s.shared(); (sh.frame.width, sh.frame.height) };
        println!("frame {w}×{h}");
        assert!(load_known_hosts(&dir.join("rdp_known_hosts")).contains_key(&host_key(&host, port)));

        // Input is accepted without ending the session.
        let key = |code, pressed| crate::rdp::tests::key(code, pressed);
        s.send(Input::Events(vec![key(0x1E, true), key(0x1E, false)]));
        s.send(Input::Events(vec![FastPathInputEvent::MouseEvent(ironrdp::pdu::input::MousePdu {
            flags: ironrdp::pdu::input::mouse::PointerFlags::MOVE,
            number_of_wheel_rotation_units: 0,
            x_position: 100,
            y_position: 100,
        })]));
        s.send(Input::LocalClipboard("hello from hostdeck".into()));
        s.send(Input::Resize { width: 1280, height: 720, scale: 100 });
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(s.shared().phase, Phase::Connected, "{:?}", s.shared().log);

        // Disconnecting ends the session normally.
        s.send(Input::Disconnect);
        wait_for(&s, "the disconnect", &|sh| matches!(sh.phase, Phase::Ended(_)));
        assert!(matches!(&s.shared().phase, Phase::Ended(e) if e.normal && e.headline == "Disconnected"));

        // A different certificate than the trusted one is refused before logging on.
        trust_certificate(&dir.join("rdp_known_hosts"), &host, port, "SHA256:00").unwrap();
        let s = RdpSession::spawn(params(&pass), ctx).unwrap();
        s.send(Input::Resize { width: 1024, height: 768, scale: 100 });
        wait_for(&s, "the certificate check", &|sh| matches!(sh.phase, Phase::Ended(_)));
        let Phase::Ended(end) = s.shared().phase.clone() else { unreachable!() };
        assert_eq!(end.headline, "Server certificate changed");
        assert!(end.untrusted_cert.is_some_and(|fp| fp.starts_with("SHA256:")));
        println!("log: {:#?}", s.shared().log);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn key(code: u8, pressed: bool) -> FastPathInputEvent {
        use ironrdp::pdu::input::fast_path::KeyboardFlags;
        FastPathInputEvent::KeyboardEvent(if pressed { KeyboardFlags::empty() } else { KeyboardFlags::RELEASE }, code)
    }

    #[test]
    fn certificate_policy() {
        let dir = std::env::temp_dir().join(format!("hostdeck-test-{}", uuid::Uuid::new_v4()));
        let shared = Mutex::new(Shared { phase: Phase::Connecting, frame: Frame::default(), log: Vec::new(), remote_clipboard: None });
        let mut p = Params {
            host: "win01".into(),
            port: 3389,
            username: String::new(),
            domain: String::new(),
            password: String::new(),
            clipboard: false,
            cert_policy: CertPolicy::Deny,
            known_hosts: dir.join("rdp_known_hosts"),
        };
        // Deny refuses an unknown certificate and offers it for trust.
        let end = check_certificate(&p, "SHA256:aa", &shared).unwrap_err();
        assert_eq!(end.untrusted_cert.as_deref(), Some("SHA256:aa"));
        // Trust on first use stores it, then accepts it and refuses a different one.
        p.cert_policy = CertPolicy::Tofu;
        assert!(check_certificate(&p, "SHA256:aa", &shared).is_ok());
        assert!(check_certificate(&p, "SHA256:aa", &shared).is_ok());
        let end = check_certificate(&p, "SHA256:bb", &shared).unwrap_err();
        assert_eq!(end.headline, "Server certificate changed");
        p.cert_policy = CertPolicy::Ignore;
        assert!(check_certificate(&p, "SHA256:bb", &shared).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
