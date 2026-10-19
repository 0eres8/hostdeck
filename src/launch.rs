//! Turns sessions into OpenSSH and FreeRDP command lines, and supervises RDP processes.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use eframe::egui;

use crate::model::{CertPolicy, ForwardKind, RdpAudio, RdpResolution, Session};

/// Arguments for `ssh`. Authentication prompts stay in the terminal so host-key
/// confirmation, OTP and keyboard-interactive logins work exactly as in OpenSSH.
pub fn ssh_args(s: &Session, username: &str) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let o = &s.ssh;
    if s.port != 22 && s.port != 0 {
        a.extend(["-p".into(), s.port.to_string()]);
    }
    if !username.is_empty() {
        a.extend(["-l".into(), username.to_string()]);
    }
    if !o.identity_file.trim().is_empty() {
        a.extend(["-i".into(), expand_home(o.identity_file.trim())]);
    }
    if !o.jump_host.trim().is_empty() {
        a.extend(["-J".into(), o.jump_host.trim().to_string()]);
    }
    if o.x11_forwarding {
        a.push("-X".into());
    }
    if o.agent_forwarding {
        a.push("-A".into());
    }
    if o.compression {
        a.push("-C".into());
    }
    if o.keepalive_secs > 0 {
        a.extend(["-o".into(), format!("ServerAliveInterval={}", o.keepalive_secs)]);
    }
    for f in &o.forwards {
        if f.bind_port == 0 {
            continue;
        }
        match f.kind {
            ForwardKind::Local => {
                a.extend(["-L".into(), format!("{}:{}:{}", f.bind_port, f.dest_host, f.dest_port)])
            }
            ForwardKind::Remote => {
                a.extend(["-R".into(), format!("{}:{}:{}", f.bind_port, f.dest_host, f.dest_port)])
            }
            ForwardKind::Dynamic => a.extend(["-D".into(), f.bind_port.to_string()]),
        }
    }
    a.extend(split_args(&o.extra_args));
    if !o.remote_command.trim().is_empty() {
        a.push("-t".into());
    }
    a.push(s.host.trim().to_string());
    if !o.remote_command.trim().is_empty() {
        a.push(o.remote_command.trim().to_string());
    }
    a
}

/// Arguments for `xfreerdp3`. The password is passed on stdin, never on the command line.
pub fn rdp_args(s: &Session, username: &str, domain: &str) -> Vec<String> {
    let o = &s.rdp;
    let mut a = vec![format!("/v:{}", s.host.trim())];
    if s.port != 3389 && s.port != 0 {
        a.push(format!("/port:{}", s.port));
    }
    if !username.is_empty() {
        a.push(format!("/u:{username}"));
    }
    // Pass a domain so /from-stdin only ever asks for the password, unless the username
    // already carries one (DOMAIN\user or user@domain), which an empty /d: would clear.
    if !domain.is_empty() || !username.contains(['\\', '@']) {
        a.push(format!("/d:{domain}"));
    }
    a.push("/from-stdin:force".into());
    a.push(format!("/t:{} — Hostdeck", s.display_name()));
    a.push("/wm-class:hostdeck-rdp".into());
    a.push(
        match o.cert_policy {
            CertPolicy::Tofu => "/cert:tofu",
            CertPolicy::Ignore => "/cert:ignore",
            CertPolicy::Deny => "/cert:deny",
        }
        .into(),
    );
    match o.resolution {
        RdpResolution::FitWindow => {
            a.push(format!("/size:{}x{}", o.width.max(640), o.height.max(480)));
            a.push("+dynamic-resolution".into());
        }
        RdpResolution::Fullscreen => {
            a.push("+f".into());
            // A bar at the top edge with minimize, windowed-mode and close buttons, so a
            // fullscreen session can always be left with the mouse.
            a.push("/floatbar:sticky:off,default:visible,show:fullscreen".into());
            if o.multimon {
                a.push("+multimon".into());
            }
        }
        RdpResolution::Fixed => a.push(format!("/size:{}x{}", o.width.max(640), o.height.max(480))),
    }
    if !o.clipboard {
        a.push("-clipboard".into());
    }
    if o.redirect_home
        && let Some(home) = dirs::home_dir() {
            a.push(format!("/drive:Home,{}", home.display()));
        }
    match o.audio {
        RdpAudio::Local => a.push("/sound".into()),
        RdpAudio::Remote => a.push("/audio-mode:1".into()),
        RdpAudio::Off => a.push("/audio-mode:2".into()),
    }
    if o.admin {
        a.push("+admin".into());
    }
    if !o.gateway.trim().is_empty() {
        let mut g = format!("/gateway:g:{}", o.gateway.trim());
        if !o.gateway_user.trim().is_empty() {
            g.push_str(&format!(",u:{}", o.gateway_user.trim()));
        }
        a.push(g);
    }
    if matches!(o.scale, 140 | 180) {
        a.push(format!("/scale:{}", o.scale));
    }
    a.push("+auto-reconnect".into());
    a.push("/log-level:WARN".into());
    a.extend(split_args(&o.extra_args));
    a
}

pub fn split_args(s: &str) -> Vec<String> {
    if s.trim().is_empty() {
        return Vec::new();
    }
    shlex::split(s).unwrap_or_else(|| s.split_whitespace().map(String::from).collect())
}

pub fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest).display().to_string(),
        _ => p.to_string(),
    }
}

pub fn find_program(candidates: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    candidates.iter().find_map(|name| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|p| is_executable(p))
    })
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

pub fn rdp_client() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os("HOSTDECK_RDP_CLIENT") {
        return Some(PathBuf::from(custom));
    }
    find_program(&["xfreerdp3", "xfreerdp"])
}

pub struct RdpProcess {
    child: Child,
    log: Arc<Mutex<Vec<String>>>,
    exit: Option<ExitStatus>,
}

impl RdpProcess {
    pub fn spawn(
        program: &Path,
        args: &[String],
        password: &str,
        repaint: egui::Context,
    ) -> std::io::Result<Self> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            // FreeRDP reads exactly one line for the password and then stops reading stdin.
            let _ = writeln!(stdin, "{password}");
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        for stream in [
            child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        {
            let log = log.clone();
            let repaint = repaint.clone();
            thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let mut l = log.lock().unwrap();
                    l.push(line);
                    if l.len() > 500 {
                        l.drain(..100);
                    }
                    repaint.request_repaint();
                }
                repaint.request_repaint();
            });
        }
        Ok(Self { child, log, exit: None })
    }

    pub fn poll(&mut self) -> Option<ExitStatus> {
        if self.exit.is_none() {
            self.exit = self.child.try_wait().ok().flatten();
        }
        self.exit
    }

    /// Exit status as of the last [`RdpProcess::poll`].
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit
    }

    pub fn kill(&mut self) {
        if self.poll().is_none() {
            let _ = self.child.kill();
            self.exit = self.child.wait().ok();
        }
    }

    pub fn log_lines(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl Drop for RdpProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A human explanation of the most likely reason an RDP connection ended.
pub fn explain_rdp_log(log: &[String]) -> Option<&'static str> {
    let text = log.join("\n");
    let rules: &[(&str, &str)] = &[
        ("ERRCONNECT_LOGON_FAILURE", "The server rejected the username or password."),
        ("ERRCONNECT_PASSWORD_EXPIRED", "The password has expired. Change it on the server first."),
        ("ERRCONNECT_PASSWORD_MUST_CHANGE", "The password must be changed before logging on."),
        ("ERRCONNECT_ACCOUNT_LOCKED_OUT", "The account is locked out."),
        ("ERRCONNECT_ACCOUNT_DISABLED", "The account is disabled."),
        ("ERRCONNECT_DNS_NAME_NOT_FOUND", "The host name could not be resolved."),
        ("ERRCONNECT_CONNECT_TRANSPORT_FAILED", "Could not reach the server. Check the host, port and firewall."),
        ("ERRCONNECT_CONNECT_FAILED", "Could not reach the server. Check the host, port and firewall."),
        ("ERRCONNECT_TLS_CONNECT_FAILED", "The TLS handshake failed. Try a different security setting in extra arguments."),
        ("REMOTE HOST IDENTIFICATION HAS CHANGED", "The server certificate changed since the last connection. Verify it, then remove the old entry in ~/.config/freerdp/server/."),
        ("certificate", "The server certificate was not accepted. Check the certificate policy."),
        ("ERRINFO_LOGOFF_BY_USER", "You logged off."),
        ("ERRINFO_RPC_INITIATED_DISCONNECT", "The server ended the session."),
        ("ERRINFO_DISCONNECTED_BY_OTHERCONNECTION", "Another connection took over this session."),
    ];
    rules.iter().find(|(needle, _)| text.contains(needle)).map(|(_, why)| *why)
}

pub struct RdpEnd {
    /// The session ended normally (logoff, disconnect, idle) rather than failing.
    pub normal: bool,
    pub headline: &'static str,
    pub detail: Option<&'static str>,
}

/// Interpret how an RDP client process ended, using FreeRDP's documented exit codes
/// (`XF_EXIT_*` in client/X11/xfreerdp.h) and falling back to its log.
pub fn explain_rdp_end(code: Option<i32>, log: &[String], user_disconnected: bool) -> RdpEnd {
    let ok = |headline, detail| RdpEnd { normal: true, headline, detail };
    let fail = |detail| RdpEnd { normal: false, headline: "Connection failed", detail };
    if user_disconnected {
        return ok("Disconnected", None);
    }
    // FreeRDP's exit code for a rejected argument varies, so recognize it by its output.
    if log.iter().any(|l| l.contains("Command line parsing failed") || l.contains("To show full command line help")) {
        return fail(Some("FreeRDP rejected the arguments. Check the extra FreeRDP arguments."));
    }
    let from_log = explain_rdp_log(log);
    match code {
        Some(0) | Some(11) => ok("Disconnected", None),
        Some(1) => ok("Disconnected", Some("The server ended the session.")),
        Some(2) => ok("Logged off", Some("You logged off.")),
        Some(3) => ok("Disconnected", Some("The session was idle for too long.")),
        Some(5) => ok("Disconnected", Some("Another connection took over this session.")),
        Some(145) => ok("Connection cancelled", None),
        Some(4) => fail(Some("The logon timed out.")),
        Some(7) | Some(155) => fail(Some("The server denied access.")),
        Some(9) | Some(144) | Some(158) => {
            fail(Some("This account may not log on remotely. Add it to the Remote Desktop Users group on the server."))
        }
        Some(16..=26) => fail(Some("The server has a Remote Desktop licensing problem.")),
        Some(128) => fail(Some("FreeRDP rejected the arguments. Check the extra FreeRDP arguments.")),
        Some(131) | Some(141) | Some(147) => fail(Some("Could not reach the server. Check the host, port and firewall.")),
        Some(132) | Some(134) | Some(154) => fail(Some("The server rejected the username or password.")),
        Some(133) | Some(161) => fail(Some("Security negotiation failed. The server may require NLA, or try /sec:tls in extra arguments.")),
        Some(135) => fail(Some("The account is locked out.")),
        Some(139) | Some(140) => fail(Some("The host name could not be resolved.")),
        Some(143) => fail(Some("The TLS handshake failed.")),
        Some(148) | Some(152) => fail(Some("The password has expired. Change it on the server first.")),
        Some(149) => fail(Some("The password must be changed before logging on.")),
        Some(150) => fail(Some("The Kerberos domain controller could not be reached.")),
        Some(151) => fail(Some("The account is disabled.")),
        Some(156) => fail(Some("An account restriction prevents logging on, for example a blank password.")),
        Some(157) => fail(Some("The account has expired.")),
        Some(159) => fail(Some("No credentials were provided.")),
        Some(160) => fail(Some("The remote computer is still starting. Try again in a moment.")),
        None => fail(from_log.or(Some("FreeRDP stopped unexpectedly."))),
        Some(_) => fail(from_log),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PortForward, Protocol};

    #[test]
    fn ssh_args_full() {
        let mut s = Session::new(Protocol::Ssh, "");
        s.host = "db1.example.com".into();
        s.port = 2222;
        s.ssh.identity_file = "/keys/id_ed25519".into();
        s.ssh.jump_host = "ops@bastion".into();
        s.ssh.x11_forwarding = true;
        s.ssh.forwards.push(PortForward {
            kind: ForwardKind::Local,
            bind_port: 5433,
            dest_host: "localhost".into(),
            dest_port: 5432,
        });
        s.ssh.remote_command = "htop".into();
        let a = ssh_args(&s, "admin");
        assert_eq!(
            a,
            [
                "-p", "2222", "-l", "admin", "-i", "/keys/id_ed25519", "-J", "ops@bastion", "-X",
                "-o", "ServerAliveInterval=60", "-L", "5433:localhost:5432", "-t",
                "db1.example.com", "htop"
            ]
        );
    }

    #[test]
    fn rdp_args_never_contain_password() {
        let mut s = Session::new(Protocol::Rdp, "");
        s.host = "win01".into();
        s.rdp.domain = "CORP".into();
        let a = rdp_args(&s, "alice", "CORP");
        assert!(a.contains(&"/v:win01".to_string()));
        assert!(a.contains(&"/u:alice".to_string()));
        assert!(a.contains(&"/d:CORP".to_string()));
        assert!(a.iter().all(|x| !x.starts_with("/p:")));
        assert!(a.iter().all(|x| !x.starts_with("/floatbar")));
        s.rdp.resolution = RdpResolution::Fullscreen;
        let a = rdp_args(&s, "alice", "CORP");
        assert!(a.contains(&"+f".to_string()));
        assert!(a.iter().any(|x| x.starts_with("/floatbar:")));
        // A domain inside the username must not be cleared by an empty /d:.
        let a = rdp_args(&s, "CORP\\alice", "");
        assert!(a.iter().all(|x| !x.starts_with("/d:")));
    }

    #[test]
    fn rdp_exit_codes() {
        let e = explain_rdp_end(Some(2), &[], false);
        assert!(e.normal);
        let e = explain_rdp_end(Some(132), &[], false);
        assert!(!e.normal);
        assert_eq!(e.detail, Some("The server rejected the username or password."));
        assert!(explain_rdp_end(None, &[], true).normal);
        let bad_args = vec!["xfreerdp3 /?".to_string(), "To show full command line help type".to_string()];
        assert_eq!(
            explain_rdp_end(Some(22), &bad_args, false).detail,
            Some("FreeRDP rejected the arguments. Check the extra FreeRDP arguments.")
        );
        assert!(!explain_rdp_end(None, &[], false).normal);
    }

    #[test]
    fn explains_logon_failure() {
        let log = vec!["[ERROR][com.freerdp.core] - ERRCONNECT_LOGON_FAILURE [0x00020014]".to_string()];
        assert_eq!(explain_rdp_log(&log), Some("The server rejected the username or password."));
    }
}
