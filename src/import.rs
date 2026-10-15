//! Importers for MobaXterm session exports, OpenSSH client config, and Hostdeck's own export.

use std::path::Path;

use anyhow::{Context, Result};

use crate::model::{join_folder, Protocol, Session, Store};

pub struct ImportReport {
    pub sessions: Vec<Session>,
    pub folders: Vec<String>,
    pub skipped: Vec<String>,
}

/// Decode a MobaXterm INI file, which is usually Windows-1252 rather than UTF-8.
fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

fn unescape_moba(s: &str) -> String {
    s.replace("__DIEZE__", "#")
        .replace("__PTVIRG__", ";")
        .replace("__PIPE__", "|")
        .replace("__DBLQUO__", "\"")
        .replace("__PERCENT__", "%")
}

fn moba_path(s: &str) -> String {
    let s = unescape_moba(s).replace('\\', "/");
    match s.strip_prefix("_ProfileDir_/") {
        Some(rest) => format!("~/{rest}"),
        None => s,
    }
}

/// Parse a MobaXterm `.mxtsessions` export. SSH and RDP bookmarks are imported; other
/// session types are reported as skipped.
pub fn parse_mobaxterm(text: &str, base_folder: &str) -> ImportReport {
    let mut report = ImportReport { sessions: Vec::new(), folders: Vec::new(), skipped: Vec::new() };
    let mut folder = base_folder.to_string();
    let mut in_bookmarks = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = &line[1..line.len() - 1];
            in_bookmarks = name == "Bookmarks" || name.starts_with("Bookmarks_");
            folder = base_folder.to_string();
            continue;
        }
        if !in_bookmarks {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        match key {
            "SubRep" => {
                let sub = unescape_moba(value).replace('\\', "/");
                folder = join_folder(base_folder, sub.trim_matches('/'));
                if !folder.is_empty() {
                    report.folders.push(folder.clone());
                }
            }
            "ImgNum" => {}
            name => {
                let parts: Vec<&str> = value.split('#').collect();
                let Some(fields) = parts.get(2) else { continue };
                let fields: Vec<&str> = fields.split('%').collect();
                let protocol = match fields.first().copied() {
                    Some("0") => Protocol::Ssh,
                    Some("4") => Protocol::Rdp,
                    _ => {
                        report.skipped.push(format!("{name} (unsupported session type)"));
                        continue;
                    }
                };
                let host = unescape_moba(fields.get(1).copied().unwrap_or("")).trim().to_string();
                if host.is_empty() {
                    report.skipped.push(format!("{name} (no host)"));
                    continue;
                }
                let mut s = Session::new(protocol, &folder);
                s.name = unescape_moba(name);
                s.host = host;
                s.port = fields
                    .get(2)
                    .and_then(|p| p.trim().parse().ok())
                    .filter(|p| *p > 0)
                    .unwrap_or(protocol.default_port());
                s.username = unescape_moba(fields.get(3).copied().unwrap_or("")).trim().to_string();
                if protocol == Protocol::Rdp
                    && let Some((domain, user)) = s.username.clone().split_once('\\')
                {
                    s.rdp.domain = domain.to_string();
                    s.username = user.to_string();
                }
                if protocol == Protocol::Ssh
                    && let Some(key) = fields.iter().map(|f| moba_path(f)).find(|f| looks_like_key(f)) {
                        if key.to_lowercase().ends_with(".ppk") {
                            s.notes = format!(
                                "MobaXterm used the PuTTY key {key}. Convert it with \
                                 `puttygen key.ppk -O private-openssh -o ~/.ssh/id_converted` and set it here."
                            );
                        } else {
                            s.ssh.identity_file = key;
                        }
                    }
                if s.notes.is_empty() {
                    s.notes = "Imported from MobaXterm".into();
                }
                report.sessions.push(s);
            }
        }
    }
    report
}

fn looks_like_key(f: &str) -> bool {
    let l = f.to_lowercase();
    !l.is_empty()
        && (l.contains(".ssh/") || l.ends_with(".ppk") || l.ends_with(".pem") || l.contains("id_rsa") || l.contains("id_ed25519") || l.contains("id_ecdsa"))
}

/// Parse an OpenSSH client config. Each concrete `Host` alias becomes a session that
/// connects through the alias, so every option in the config keeps applying.
pub fn parse_ssh_config(text: &str, folder: &str) -> ImportReport {
    let mut report = ImportReport { sessions: Vec::new(), folders: vec![folder.to_string()], skipped: Vec::new() };
    let mut current: Vec<Session> = Vec::new();
    let flush = |current: &mut Vec<Session>, report: &mut ImportReport| report.sessions.append(current);

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.to_lowercase(), v.trim().trim_start_matches('=').trim().to_string()),
            None => continue,
        };
        match key.as_str() {
            "host" => {
                flush(&mut current, &mut report);
                for alias in value.split_whitespace() {
                    if alias.contains(['*', '?', '!']) {
                        report.skipped.push(format!("{alias} (pattern)"));
                        continue;
                    }
                    let mut s = Session::new(Protocol::Ssh, folder);
                    s.name = alias.to_string();
                    s.host = alias.to_string();
                    s.notes = "Imported from ~/.ssh/config; options from that file still apply.".into();
                    current.push(s);
                }
            }
            "match" => flush(&mut current, &mut report),
            "hostname" => {
                for s in &mut current {
                    s.notes = format!("HostName {value}. {}", s.notes);
                }
            }
            "user" => current.iter_mut().for_each(|s| s.notes = format!("User {value}. {}", s.notes)),
            _ => {}
        }
    }
    flush(&mut current, &mut report);
    report
}

pub fn read_mobaxterm(path: &Path, folder: &str) -> Result<ImportReport> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(parse_mobaxterm(&decode(&bytes), folder))
}

pub fn read_ssh_config(path: &Path, folder: &str) -> Result<ImportReport> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(parse_ssh_config(&text, folder))
}

pub fn read_hostdeck(path: &Path) -> Result<ImportReport> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let store: Store = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(ImportReport { sessions: store.sessions, folders: store.folders.into_iter().collect(), skipped: Vec::new() })
}

/// Merge imported sessions, skipping ones that already exist with the same target.
pub fn merge(store: &mut Store, report: ImportReport) -> (usize, usize) {
    let mut added = 0;
    let mut duplicates = 0;
    for f in report.folders.into_iter().filter(|f| !f.is_empty()) {
        store.folders.insert(f);
    }
    for mut s in report.sessions {
        let exists = store.sessions.iter().any(|e| {
            e.protocol == s.protocol && e.host == s.host && e.port == s.port && e.username == s.username && e.folder == s.folder
        });
        if exists {
            duplicates += 1;
            continue;
        }
        if store.session(s.id).is_some() {
            s.id = uuid::Uuid::new_v4();
        }
        // Credentials from another machine are not imported, so drop dangling references.
        if s.credential.is_some_and(|c| store.credential(c).is_none()) {
            s.credential = None;
        }
        store.sessions.push(s);
        added += 1;
    }
    (added, duplicates)
}

pub fn export_hostdeck(store: &Store, path: &Path) -> Result<()> {
    let export = Store {
        version: 1,
        settings: Default::default(),
        folders: store.folders.clone(),
        credentials: Vec::new(),
        sessions: store
            .sessions
            .iter()
            .cloned()
            .map(|mut s| {
                s.credential = None;
                s.save_password = false;
                s
            })
            .collect(),
    };
    crate::model::write_private(path, &toml::to_string_pretty(&export)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOBA: &str = "[Bookmarks]\r\nSubRep=\r\nImgNum=42\r\nlab=#109#0%192.168.1.10%2222%root%%-1%-1%%%22%%0%0%0%_ProfileDir_\\.ssh\\id_ed25519%%-1%0%0%0%%1080%%0%0%1#MobaFont%10%0%0%-1%15#0# #-1\r\n\r\n[Bookmarks_1]\r\nSubRep=Production\\Windows\r\nImgNum=41\r\nwin01=#91#4%10.0.0.20%3389%CORP\\administrator%0%0%0%0%-1%0%0%-1#MobaFont#0# #-1\r\nserial=#131#8%COM3%115200%8#MobaFont#0# #-1\r\n";

    #[test]
    fn mobaxterm_ssh_and_rdp() {
        let r = parse_mobaxterm(MOBA, "MobaXterm");
        assert_eq!(r.sessions.len(), 2);
        let ssh = &r.sessions[0];
        assert_eq!((ssh.name.as_str(), ssh.host.as_str(), ssh.port, ssh.username.as_str()), ("lab", "192.168.1.10", 2222, "root"));
        assert_eq!(ssh.folder, "MobaXterm");
        assert_eq!(ssh.ssh.identity_file, "~/.ssh/id_ed25519");
        let rdp = &r.sessions[1];
        assert_eq!(rdp.protocol, Protocol::Rdp);
        assert_eq!(rdp.folder, "MobaXterm/Production/Windows");
        assert_eq!(rdp.username, "administrator");
        assert_eq!(rdp.rdp.domain, "CORP");
        assert_eq!(r.skipped.len(), 1);
    }

    #[test]
    fn ssh_config_aliases() {
        let cfg = "Host *\n  ServerAliveInterval 30\nHost db1 db2\n  HostName db.example.com\n  User ops\nHost=bastion\n  Port 2200\n";
        let r = parse_ssh_config(cfg, "SSH config");
        let names: Vec<_> = r.sessions.iter().map(|s| s.host.as_str()).collect();
        assert_eq!(names, ["db1", "db2", "bastion"]);
        assert!(r.sessions[0].notes.contains("db.example.com"));
    }
}
