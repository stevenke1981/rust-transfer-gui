//! FTP back-end built on the synchronous API of the [`suppaftp`] crate.

use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use suppaftp::list::ListParser;
use suppaftp::types::FileType;
use suppaftp::{FtpStream, Mode};

use crate::common::{ProgressFn, RemoteEntry, copy_with_progress, resolve};

/// Default FTP control port.
pub const DEFAULT_PORT: u16 = 21;

/// Connection parameters. The password lives only in memory.
#[derive(Clone)]
pub struct FtpConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// Use passive mode (PASV). If false, active mode (PORT) is used.
    pub passive: bool,
    pub timeout: Duration,
}

impl std::fmt::Debug for FtpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("passive", &self.passive)
            .finish()
    }
}

impl Default for FtpConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_PORT,
            username: "anonymous".into(),
            password: String::new(),
            passive: true,
            timeout: Duration::from_secs(15),
        }
    }
}

/// A logged-in FTP session.
pub struct FtpClient {
    stream: FtpStream,
}

impl FtpClient {
    /// Connect, log in and switch to binary transfer type.
    pub fn connect(cfg: &FtpConfig) -> Result<Self> {
        let addr = resolve(&cfg.host, cfg.port)?;
        let mut stream =
            FtpStream::connect_timeout(addr, cfg.timeout).with_context(|| format!("cannot connect to {addr}"))?;
        let user = if cfg.username.trim().is_empty() { "anonymous" } else { cfg.username.trim() };
        stream.login(user, cfg.password.as_str()).context("login failed")?;
        stream.set_mode(if cfg.passive { Mode::Passive } else { Mode::Active });
        stream.transfer_type(FileType::Binary).context("cannot set binary mode")?;
        Ok(Self { stream })
    }

    /// Server welcome banner, if any.
    pub fn welcome(&self) -> Option<String> {
        self.stream.get_welcome_msg().map(|s| s.trim().to_string())
    }

    /// Current remote working directory.
    pub fn pwd(&mut self) -> Result<String> {
        self.stream.pwd().context("PWD failed")
    }

    /// Change into `path` (absolute or relative).
    pub fn cwd(&mut self, path: &str) -> Result<()> {
        self.stream.cwd(path).with_context(|| format!("cannot change directory to {path}"))
    }

    /// Go to the parent directory.
    pub fn cdup(&mut self) -> Result<()> {
        self.stream.cdup().context("CDUP failed")
    }

    /// List the current directory.
    pub fn list(&mut self) -> Result<Vec<RemoteEntry>> {
        let lines = self.stream.list(None).context("LIST failed")?;
        let mut entries: Vec<RemoteEntry> = lines.iter().filter_map(|l| parse_list_line(l)).collect();
        if entries.is_empty() && !lines.is_empty() {
            // Unknown LIST format: fall back to bare names.
            let names = self.stream.nlst(None).context("NLST failed")?;
            entries = names
                .into_iter()
                .map(|n| n.rsplit('/').next().unwrap_or(&n).to_string())
                .filter(|n| n != "." && n != "..")
                .map(|name| RemoteEntry { name, is_dir: false, size: None, modified: None })
                .collect();
        }
        RemoteEntry::sort_listing(&mut entries);
        Ok(entries)
    }

    /// Upload a local file into the current remote directory as `remote_name`.
    pub fn upload(&mut self, local: &Path, remote_name: &str, progress: &mut ProgressFn<'_>) -> Result<u64> {
        let file = File::open(local).with_context(|| format!("cannot open {}", local.display()))?;
        let total = file.metadata().ok().map(|m| m.len());
        let mut reader = BufReader::new(file);
        let mut data = self
            .stream
            .put_with_stream(remote_name)
            .with_context(|| format!("server refused upload of {remote_name}"))?;
        let n = copy_with_progress(&mut reader, &mut data, total, progress).context("upload failed")?;
        data.finish().context("server did not confirm the upload")?;
        Ok(n)
    }

    /// Download `remote_name` (relative to the cwd or absolute) into `local`.
    pub fn download(&mut self, remote_name: &str, local: &Path, progress: &mut ProgressFn<'_>) -> Result<u64> {
        let total = self.stream.size(remote_name).ok().map(|s| s as u64);
        let file = File::create(local).with_context(|| format!("cannot create {}", local.display()))?;
        let mut writer = BufWriter::new(file);
        let res = self.stream.retr(remote_name, |r| {
            copy_with_progress(r, &mut writer, total, progress).map_err(suppaftp::FtpError::ConnectionError)
        });
        let res = res
            .map_err(|e| anyhow!(e))
            .and_then(|n| writer.flush().map(|_| n).map_err(Into::into))
            .with_context(|| format!("download of {remote_name} failed"));
        if res.is_err() {
            drop(writer);
            let _ = std::fs::remove_file(local);
        }
        res
    }

    /// Create a directory (relative to the cwd or absolute).
    pub fn mkdir(&mut self, name: &str) -> Result<()> {
        self.stream.mkdir(name).with_context(|| format!("cannot create directory {name}"))
    }

    /// Delete a file, or an (empty) directory if `is_dir`.
    pub fn delete(&mut self, name: &str, is_dir: bool) -> Result<()> {
        if is_dir {
            self.stream.rmdir(name).with_context(|| format!("cannot remove directory {name} (is it empty?)"))
        } else {
            self.stream.rm(name).with_context(|| format!("cannot delete {name}"))
        }
    }

    /// Log out politely.
    pub fn quit(mut self) -> Result<()> {
        self.stream.quit().context("QUIT failed")
    }
}

/// Parse one line of `LIST` output (POSIX `ls -l` style or DOS/IIS style).
pub fn parse_list_line(line: &str) -> Option<RemoteEntry> {
    let f = ListParser::parse_posix(line).or_else(|_| ListParser::parse_dos(line)).ok()?;
    let name = f.name().to_string();
    if name == "." || name == ".." || name.is_empty() {
        return None;
    }
    // Whether a symlink points to a directory is unknown from LIST output; offer it as
    // enterable and let CWD fail gracefully if it is actually a file.
    let is_dir = f.is_directory() || f.is_symlink();
    let modified = f.modified().duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()).filter(|&s| s > 0);
    Some(RemoteEntry { name, is_dir, size: if f.is_directory() { None } else { Some(f.size() as u64) }, modified })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_posix_lines() {
        let e = parse_list_line("-rw-r--r--   1 user  group     1234 Jan 01 12:00 file.txt").unwrap();
        assert_eq!(e.name, "file.txt");
        assert!(!e.is_dir);
        assert_eq!(e.size, Some(1234));
        assert!(e.modified.is_some());
        let d = parse_list_line("drwxr-xr-x   2 user  group     4096 Jan 01 12:00 my dir").unwrap();
        assert!(d.is_dir);
        assert_eq!(d.name, "my dir");
        assert!(parse_list_line("drwxr-xr-x   2 user  group     4096 Jan 01 12:00 .").is_none());
    }

    #[test]
    fn parses_dos_lines() {
        let d = parse_list_line("10-19-20  03:19PM <DIR> pub").unwrap();
        assert!(d.is_dir);
        let f = parse_list_line("04-08-14  03:09PM 403   readme.txt").unwrap();
        assert_eq!(f.size, Some(403));
    }

    #[test]
    fn debug_redacts_password() {
        let c = FtpConfig { password: "hunter2".into(), ..Default::default() };
        assert!(!format!("{c:?}").contains("hunter2"));
    }
}
