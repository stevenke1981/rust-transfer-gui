//! Saved sessions and settings, stored in a small INI-like text file.
//!
//! **Passwords and passphrases are never written** — the [`SavedSession`] type
//! simply has no field for them.
//!
//! Location (override with the `RUST_TRANSFER_GUI_CONFIG` environment variable):
//! * Linux/BSD: `$XDG_CONFIG_HOME/rust-transfer-gui/sessions.conf` (default `~/.config/...`)
//! * macOS: `~/Library/Application Support/rust-transfer-gui/sessions.conf`
//! * Windows: `%APPDATA%\rust-transfer-gui\sessions.conf`

use std::fmt::Write as _;
use std::path::PathBuf;

/// Supported protocols / session types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    Ssh,
    Sftp,
    Ftp,
    Tftp,
}

impl Protocol {
    pub const ALL: [Protocol; 4] = [Protocol::Ssh, Protocol::Sftp, Protocol::Ftp, Protocol::Tftp];

    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ssh => "SSH",
            Protocol::Sftp => "SFTP",
            Protocol::Ftp => "FTP",
            Protocol::Tftp => "TFTP",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Protocol::Ssh => "🖥",
            Protocol::Sftp => "📂",
            Protocol::Ftp => "🌐",
            Protocol::Tftp => "📡",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Protocol::Ssh | Protocol::Sftp => crate::sftp::DEFAULT_PORT,
            Protocol::Ftp => crate::ftp::DEFAULT_PORT,
            Protocol::Tftp => crate::tftp::DEFAULT_PORT,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Protocol::ALL.into_iter().find(|p| p.label().eq_ignore_ascii_case(s.trim()))
    }
}

/// Authentication method remembered for SSH/SFTP sessions (never the secret itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMethod {
    #[default]
    Password,
    KeyFile,
}

/// A saved session (bookmark). Contains no secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedSession {
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    pub key_path: String,
    /// FTP only: passive mode.
    pub passive: bool,
}

impl SavedSession {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            name: String::new(),
            protocol,
            host: String::new(),
            port: protocol.default_port(),
            user: String::new(),
            auth: AuthMethod::Password,
            key_path: String::new(),
            passive: true,
        }
    }

    /// A display name: the explicit name, or `user@host`.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            self.name.trim().to_string()
        } else if self.user.trim().is_empty() || self.protocol == Protocol::Tftp {
            self.host.clone()
        } else {
            format!("{}@{}", self.user.trim(), self.host)
        }
    }
}

/// Application settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub dark_mode: bool,
    pub show_sidebar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { dark_mode: true, show_sidebar: true }
    }
}

/// Everything persisted to disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub settings: Settings,
    pub sessions: Vec<SavedSession>,
}

fn clean(v: &str) -> String {
    v.replace(['\n', '\r'], " ")
}

impl Config {
    /// Serialize to the INI-like text format.
    pub fn to_text(&self) -> String {
        let mut out = String::from(
            "# rust-transfer-gui configuration\n# Passwords and passphrases are never stored in this file.\n\n",
        );
        let _ = writeln!(out, "[settings]");
        let _ = writeln!(out, "theme={}", if self.settings.dark_mode { "dark" } else { "light" });
        let _ = writeln!(out, "sidebar={}", self.settings.show_sidebar);
        for s in &self.sessions {
            let _ = writeln!(out, "\n[session]");
            let _ = writeln!(out, "name={}", clean(&s.name));
            let _ = writeln!(out, "protocol={}", s.protocol.label());
            let _ = writeln!(out, "host={}", clean(&s.host));
            let _ = writeln!(out, "port={}", s.port);
            let _ = writeln!(out, "user={}", clean(&s.user));
            let _ = writeln!(
                out,
                "auth={}",
                match s.auth {
                    AuthMethod::Password => "password",
                    AuthMethod::KeyFile => "key",
                }
            );
            let _ = writeln!(out, "key={}", clean(&s.key_path));
            let _ = writeln!(out, "passive={}", s.passive);
        }
        out
    }

    /// Parse the text format. Unknown keys and malformed lines are ignored.
    pub fn from_text(text: &str) -> Self {
        enum Section {
            None,
            Settings,
            Session,
        }
        let mut cfg = Config::default();
        let mut section = Section::None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.eq_ignore_ascii_case("[settings]") {
                section = Section::Settings;
                continue;
            }
            if line.eq_ignore_ascii_case("[session]") {
                section = Section::Session;
                cfg.sessions.push(SavedSession::new(Protocol::Ssh));
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match section {
                Section::Settings => match key.as_str() {
                    "theme" => cfg.settings.dark_mode = !value.eq_ignore_ascii_case("light"),
                    "sidebar" => cfg.settings.show_sidebar = value != "false",
                    _ => {}
                },
                Section::Session => {
                    let Some(s) = cfg.sessions.last_mut() else { continue };
                    match key.as_str() {
                        "name" => s.name = value.to_string(),
                        "protocol" => {
                            if let Some(p) = Protocol::parse(value) {
                                s.protocol = p;
                            }
                        }
                        "host" => s.host = value.to_string(),
                        "port" => s.port = value.parse().unwrap_or(s.port),
                        "user" => s.user = value.to_string(),
                        "auth" => s.auth = if value == "key" { AuthMethod::KeyFile } else { AuthMethod::Password },
                        "key" => s.key_path = value.to_string(),
                        "passive" => s.passive = value != "false",
                        // Deliberately ignore anything that looks like a secret.
                        _ => {}
                    }
                }
                Section::None => {}
            }
        }
        cfg.sessions.retain(|s| !s.host.is_empty());
        cfg
    }

    /// Default config file path for this platform.
    pub fn path() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("RUST_TRANSFER_GUI_CONFIG") {
            return Some(PathBuf::from(p));
        }
        let base = if cfg!(windows) {
            std::env::var_os("APPDATA").map(PathBuf::from)
        } else if cfg!(target_os = "macos") {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support"))
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        }?;
        Some(base.join("rust-transfer-gui").join("sessions.conf"))
    }

    /// Load from the default location (missing/unreadable file → defaults).
    pub fn load() -> Self {
        Self::path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| Self::from_text(&t)).unwrap_or_default()
    }

    /// Save to the default location.
    pub fn save(&self) -> std::io::Result<PathBuf> {
        let path =
            Self::path().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config directory"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, self.to_text())?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            settings: Settings { dark_mode: false, show_sidebar: false },
            sessions: vec![
                SavedSession {
                    name: "prod box".into(),
                    protocol: Protocol::Ssh,
                    host: "10.0.0.5".into(),
                    port: 2222,
                    user: "root".into(),
                    auth: AuthMethod::KeyFile,
                    key_path: "/home/u/.ssh/id_ed25519".into(),
                    passive: true,
                },
                SavedSession {
                    name: String::new(),
                    protocol: Protocol::Ftp,
                    host: "ftp.example.com".into(),
                    port: 21,
                    user: "anonymous".into(),
                    auth: AuthMethod::Password,
                    key_path: String::new(),
                    passive: false,
                },
            ],
        }
    }

    #[test]
    fn roundtrip() {
        let c = sample();
        assert_eq!(Config::from_text(&c.to_text()), c);
    }

    #[test]
    fn never_contains_password_keys() {
        let text = sample().to_text().to_lowercase();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let key = line.split('=').next().unwrap_or_default();
            assert!(!["password", "passphrase", "pass", "secret"].contains(&key), "{line}");
        }
    }

    #[test]
    fn ignores_garbage_and_secrets() {
        let c = Config::from_text("junk\n[session]\nhost=h\npassword=secret\nport=abc\n[weird]\nx=y\n");
        assert_eq!(c.sessions.len(), 1);
        assert_eq!(c.sessions[0].port, 22);
        assert!(!format!("{c:?}").contains("secret"));
    }

    #[test]
    fn newlines_cannot_inject_keys() {
        let mut c = sample();
        c.sessions[0].name = "evil\nhost=attacker".into();
        let back = Config::from_text(&c.to_text());
        assert_eq!(back.sessions[0].host, "10.0.0.5");
    }

    #[test]
    fn display_names() {
        let s = &sample().sessions;
        assert_eq!(s[0].display_name(), "prod box");
        assert_eq!(s[1].display_name(), "anonymous@ftp.example.com");
    }
}
