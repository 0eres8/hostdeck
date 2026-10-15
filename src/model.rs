//! Session, folder and credential data, persisted as TOML in the user's config directory.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Ssh,
    Rdp,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ssh => "SSH",
            Protocol::Rdp => "RDP",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh => 22,
            Protocol::Rdp => 3389,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ForwardKind {
    #[default]
    Local,
    Remote,
    Dynamic,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PortForward {
    pub kind: ForwardKind,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshOptions {
    pub identity_file: String,
    /// OpenSSH `-J` syntax: `user@host:port[,user@host2]`.
    pub jump_host: String,
    pub x11_forwarding: bool,
    pub agent_forwarding: bool,
    pub compression: bool,
    pub keepalive_secs: u32,
    pub remote_command: String,
    pub forwards: Vec<PortForward>,
    pub extra_args: String,
}

impl Default for SshOptions {
    fn default() -> Self {
        Self {
            identity_file: String::new(),
            jump_host: String::new(),
            x11_forwarding: false,
            agent_forwarding: false,
            compression: false,
            keepalive_secs: 60,
            remote_command: String::new(),
            forwards: Vec::new(),
            extra_args: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RdpResolution {
    /// Resize the remote desktop with the window.
    #[default]
    FitWindow,
    Fullscreen,
    Fixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RdpAudio {
    #[default]
    Local,
    Remote,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CertPolicy {
    /// Trust on first use, refuse if the certificate later changes.
    #[default]
    Tofu,
    Ignore,
    Deny,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RdpOptions {
    pub domain: String,
    pub resolution: RdpResolution,
    pub width: u32,
    pub height: u32,
    pub multimon: bool,
    pub clipboard: bool,
    pub redirect_home: bool,
    pub audio: RdpAudio,
    pub admin: bool,
    pub gateway: String,
    pub gateway_user: String,
    pub cert_policy: CertPolicy,
    pub scale: u32,
    pub extra_args: String,
}

impl Default for RdpOptions {
    fn default() -> Self {
        Self {
            domain: String::new(),
            resolution: RdpResolution::FitWindow,
            width: 1600,
            height: 900,
            multimon: false,
            clipboard: true,
            redirect_home: false,
            audio: RdpAudio::Local,
            admin: false,
            gateway: String::new(),
            gateway_user: String::new(),
            cert_policy: CertPolicy::Tofu,
            scale: 100,
            extra_args: String::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub id: Uuid,
    pub name: String,
    /// Slash-separated folder path, empty for the root.
    pub folder: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// When set, the username and password come from this credential.
    pub credential: Option<Uuid>,
    /// Keep this session's own password in the system keyring.
    pub save_password: bool,
    pub color: usize,
    pub favorite: bool,
    pub notes: String,
    pub ssh: SshOptions,
    pub rdp: RdpOptions,
    pub last_used: Option<u64>,
    pub use_count: u32,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            folder: String::new(),
            protocol: Protocol::Ssh,
            host: String::new(),
            port: 22,
            username: String::new(),
            credential: None,
            save_password: false,
            color: 0,
            favorite: false,
            notes: String::new(),
            ssh: SshOptions::default(),
            rdp: RdpOptions::default(),
            last_used: None,
            use_count: 0,
        }
    }
}

impl Session {
    pub fn new(protocol: Protocol, folder: &str) -> Self {
        Self {
            protocol,
            port: protocol.default_port(),
            folder: folder.to_string(),
            ..Self::default()
        }
    }

    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.clone()
        } else if !self.username.is_empty() {
            format!("{}@{}", self.username, self.host)
        } else {
            self.host.clone()
        }
    }

    pub fn endpoint(&self) -> String {
        let user = if self.username.is_empty() {
            String::new()
        } else {
            format!("{}@", self.username)
        };
        if self.port == self.protocol.default_port() {
            format!("{user}{}", self.host)
        } else {
            format!("{user}{}:{}", self.host, self.port)
        }
    }

    pub fn password_key(&self) -> String {
        format!("session:{}", self.id)
    }

    pub fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        [&self.name, &self.host, &self.username, &self.folder, &self.notes]
            .iter()
            .any(|f| f.to_lowercase().contains(&q))
    }

    pub fn touch(&mut self) {
        self.last_used = Some(now_secs());
        self.use_count += 1;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Credential {
    pub id: Uuid,
    pub name: String,
    pub username: String,
}

impl Default for Credential {
    fn default() -> Self {
        Self { id: Uuid::new_v4(), name: String::new(), username: String::new() }
    }
}

impl Credential {
    pub fn password_key(&self) -> String {
        format!("credential:{}", self.id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub terminal_font_size: f32,
    pub scrollback_lines: usize,
    pub copy_on_select: bool,
    pub right_click_paste: bool,
    pub sidebar_width: f32,
    pub collapsed_folders: BTreeSet<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::Dark,
            terminal_font_size: 14.0,
            scrollback_lines: 10_000,
            copy_on_select: true,
            right_click_paste: true,
            sidebar_width: 280.0,
            collapsed_folders: BTreeSet::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    pub version: u32,
    pub settings: Settings,
    /// Folders that exist even when empty; folders of sessions are implied.
    pub folders: BTreeSet<String>,
    pub credentials: Vec<Credential>,
    pub sessions: Vec<Session>,
}

impl Store {
    pub fn default_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("hostdeck")
            .join("hostdeck.toml")
    }

    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self { version: 1, ..Self::default() });
        }
        let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut store: Store = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        store.version = 1;
        Ok(store)
    }

    /// Write atomically with owner-only permissions.
    pub fn save(&self, path: &Path) -> Result<()> {
        write_private(path, &toml::to_string_pretty(self)?)
    }

    pub fn session(&self, id: Uuid) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn session_mut(&mut self, id: Uuid) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn credential(&self, id: Uuid) -> Option<&Credential> {
        self.credentials.iter().find(|c| c.id == id)
    }

    /// All folder paths, including every ancestor of every folder.
    pub fn all_folders(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for f in self.folders.iter().chain(self.sessions.iter().map(|s| &s.folder)) {
            let mut path = String::new();
            for part in f.split('/').filter(|p| !p.is_empty()) {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(part);
                out.insert(path.clone());
            }
        }
        out
    }

    pub fn rename_folder(&mut self, old: &str, new: &str) {
        let remap = |f: &str| -> Option<String> {
            if f == old {
                Some(new.to_string())
            } else {
                f.strip_prefix(&format!("{old}/")).map(|rest| join_folder(new, rest))
            }
        };
        self.folders = self
            .folders
            .iter()
            .map(|f| remap(f).unwrap_or_else(|| f.clone()))
            .collect();
        for s in &mut self.sessions {
            if let Some(f) = remap(&s.folder) {
                s.folder = f;
            }
        }
    }

    /// Remove a folder, its subfolders and every session inside them.
    pub fn delete_folder(&mut self, folder: &str) -> usize {
        let inside = |f: &str| f == folder || f.starts_with(&format!("{folder}/"));
        self.folders.retain(|f| !inside(f));
        let before = self.sessions.len();
        self.sessions.retain(|s| !inside(&s.folder));
        before - self.sessions.len()
    }

    pub fn sessions_in(&self, folder: &str, recursive: bool) -> Vec<Uuid> {
        self.sessions
            .iter()
            .filter(|s| {
                s.folder == folder || (recursive && s.folder.starts_with(&format!("{folder}/")))
            })
            .map(|s| s.id)
            .collect()
    }
}

/// Parse quick-connect input such as `alice@db1:2222`, `ssh://db1` or `rdp://admin@win01`.
pub fn quick_session(input: &str) -> Option<Session> {
    let input = input.trim();
    let lower = input.to_lowercase();
    let (protocol, rest) = [("rdp://", Protocol::Rdp), ("rdp ", Protocol::Rdp), ("ssh://", Protocol::Ssh), ("ssh ", Protocol::Ssh)]
        .iter()
        .find(|(prefix, _)| lower.starts_with(prefix))
        .map(|(prefix, p)| (*p, input[prefix.len()..].trim()))
        .unwrap_or((Protocol::Ssh, input));
    let (user, hostport) = match rest.rsplit_once('@') {
        Some((u, h)) => (u.to_string(), h),
        None => (String::new(), rest),
    };
    let (host, port) = if let Some(v6) = hostport.strip_prefix('[') {
        let (h, tail) = v6.split_once(']')?;
        (h.to_string(), tail.strip_prefix(':').and_then(|p| p.parse().ok()))
    } else {
        match hostport.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h.to_string(), Some(p.parse().ok()?)),
            _ => (hostport.to_string(), None),
        }
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let mut s = Session::new(protocol, "");
    s.host = host;
    s.username = user;
    s.port = port.unwrap_or(protocol.default_port());
    Some(s)
}

pub fn join_folder(parent: &str, child: &str) -> String {
    let child = child.trim_matches('/');
    if parent.is_empty() {
        child.to_string()
    } else if child.is_empty() {
        parent.to_string()
    } else {
        format!("{parent}/{child}")
    }
}

pub fn folder_leaf(folder: &str) -> &str {
    folder.rsplit('/').next().unwrap_or(folder)
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_include_ancestors() {
        let mut store = Store::default();
        store.folders.insert("Prod/Web/Edge".into());
        let mut s = Session::new(Protocol::Ssh, "Lab");
        s.host = "lab1".into();
        store.sessions.push(s);
        let all: Vec<_> = store.all_folders().into_iter().collect();
        assert_eq!(all, ["Lab", "Prod", "Prod/Web", "Prod/Web/Edge"]);
    }

    #[test]
    fn rename_folder_moves_children_only() {
        let mut store = Store::default();
        for f in ["Prod", "Prod/Web", "Production"] {
            let mut s = Session::new(Protocol::Ssh, f);
            s.host = f.into();
            store.sessions.push(s);
        }
        store.rename_folder("Prod", "Live");
        let folders: Vec<_> = store.sessions.iter().map(|s| s.folder.as_str()).collect();
        assert_eq!(folders, ["Live", "Live/Web", "Production"]);
    }

    #[test]
    fn quick_connect_forms() {
        let s = quick_session("alice@db1:2222").unwrap();
        assert_eq!((s.protocol, s.username.as_str(), s.host.as_str(), s.port), (Protocol::Ssh, "alice", "db1", 2222));
        let s = quick_session("rdp://admin@win01").unwrap();
        assert_eq!((s.protocol, s.host.as_str(), s.port), (Protocol::Rdp, "win01", 3389));
        let s = quick_session("ssh [fe80::1]:22").unwrap();
        assert_eq!(s.host, "fe80::1");
        assert!(quick_session("").is_none());
        assert!(quick_session("db1:notaport").is_none());
    }

    #[test]
    fn roundtrip_toml() {
        let mut store = Store::default();
        let mut s = Session::new(Protocol::Rdp, "Windows");
        s.host = "win01".into();
        s.rdp.domain = "CORP".into();
        store.sessions.push(s);
        let text = toml::to_string_pretty(&store).unwrap();
        let back: Store = toml::from_str(&text).unwrap();
        assert_eq!(back.sessions, store.sessions);
    }
}
