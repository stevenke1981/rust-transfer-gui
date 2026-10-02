//! SSH back-end: SFTP file transfer and remote command execution via [`ssh2`] (libssh2).

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ssh2::{HashType, Session, Sftp};

use crate::common::{ProgressFn, RemoteEntry, copy_with_progress, join_remote, resolve};

/// Default SSH port.
pub const DEFAULT_PORT: u16 = 22;

/// How to authenticate. Secrets live only in memory.
#[derive(Clone)]
pub enum SshAuth {
    Password(String),
    KeyFile { path: PathBuf, passphrase: Option<String> },
}

/// Connection parameters.
#[derive(Clone)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth: SshAuth,
    pub timeout: Duration,
}

impl std::fmt::Debug for SshConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let auth = match &self.auth {
            SshAuth::Password(_) => "password(<redacted>)".to_string(),
            SshAuth::KeyFile { path, .. } => format!("key({})", path.display()),
        };
        f.debug_struct("SshConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("auth", &auth)
            .finish()
    }
}

/// Result of a remote command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// An authenticated SSH session with an open SFTP channel.
pub struct SshClient {
    session: Session,
    sftp: Sftp,
    cwd: String,
    fingerprint: String,
}

impl SshClient {
    /// Connect, perform the handshake, authenticate and open SFTP.
    ///
    /// Note: the server host key is *not* checked against `known_hosts`; its
    /// SHA-256 fingerprint is available via [`SshClient::fingerprint`] so the UI can show it.
    pub fn connect(cfg: &SshConfig) -> Result<Self> {
        let addr = resolve(&cfg.host, cfg.port)?;
        let tcp =
            TcpStream::connect_timeout(&addr, cfg.timeout).with_context(|| format!("cannot connect to {addr}"))?;
        let mut session = Session::new().context("cannot create SSH session")?;
        session.set_tcp_stream(tcp);
        session.set_timeout(cfg.timeout.as_millis().min(u32::MAX as u128) as u32);
        session.handshake().context("SSH handshake failed")?;

        let fingerprint = session
            .host_key_hash(HashType::Sha256)
            .map(|h| format!("SHA256:{}", base64_nopad(h)))
            .unwrap_or_else(|| "unknown".into());

        let user = cfg.username.trim();
        if user.is_empty() {
            bail!("username is empty");
        }
        match &cfg.auth {
            SshAuth::Password(pw) => session.userauth_password(user, pw).context("password authentication failed")?,
            SshAuth::KeyFile { path, passphrase } => {
                if !path.is_file() {
                    bail!("private key file not found: {}", path.display());
                }
                let pass = passphrase.as_deref().filter(|p| !p.is_empty());
                session
                    .userauth_pubkey_file(user, None, path, pass)
                    .context("public key authentication failed (wrong key or passphrase?)")?
            }
        }
        if !session.authenticated() {
            bail!("authentication failed");
        }
        // Transfers can be long; individual blocking calls still time out.
        session.set_timeout(0);
        session.set_keepalive(true, 30);
        let sftp = session.sftp().context("cannot start SFTP subsystem")?;
        let cwd = sftp.realpath(Path::new(".")).map(|p| to_remote_string(&p)).unwrap_or_else(|_| "/".into());
        Ok(Self { session, sftp, cwd, fingerprint })
    }

    /// SHA-256 fingerprint of the server host key (OpenSSH style).
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Current remote directory.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// Change directory. `path` may be relative to the cwd, absolute, or `..`.
    pub fn cd(&mut self, path: &str) -> Result<()> {
        let target = join_remote(&self.cwd, path);
        let real = self.sftp.realpath(Path::new(&target)).with_context(|| format!("cannot resolve {target}"))?;
        let stat = self.sftp.stat(&real).with_context(|| format!("cannot stat {}", real.display()))?;
        if !stat.is_dir() {
            bail!("{} is not a directory", real.display());
        }
        self.cwd = to_remote_string(&real);
        Ok(())
    }

    /// Go to the parent directory.
    pub fn up(&mut self) -> Result<()> {
        self.cd("..")
    }

    /// List the current directory.
    pub fn list(&mut self) -> Result<Vec<RemoteEntry>> {
        let items = self.sftp.readdir(Path::new(&self.cwd)).with_context(|| format!("cannot list {}", self.cwd))?;
        let mut entries: Vec<RemoteEntry> = items
            .into_iter()
            .filter_map(|(path, stat)| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                if name == "." || name == ".." {
                    return None;
                }
                let is_dir = if stat.file_type().is_symlink() {
                    // Follow symlinks to decide whether they can be entered.
                    self.sftp.stat(&path).map(|s| s.is_dir()).unwrap_or(false)
                } else {
                    stat.is_dir()
                };
                Some(RemoteEntry { name, is_dir, size: if is_dir { None } else { stat.size }, modified: stat.mtime })
            })
            .collect();
        RemoteEntry::sort_listing(&mut entries);
        Ok(entries)
    }

    /// Upload `local` into the current directory as `remote_name` (or to an absolute path).
    pub fn upload(&mut self, local: &Path, remote_name: &str, progress: &mut ProgressFn<'_>) -> Result<u64> {
        let file = File::open(local).with_context(|| format!("cannot open {}", local.display()))?;
        let total = file.metadata().ok().map(|m| m.len());
        let mut reader = BufReader::new(file);
        let target = join_remote(&self.cwd, remote_name);
        let mut remote =
            self.sftp.create(Path::new(&target)).with_context(|| format!("cannot create remote file {target}"))?;
        let n = copy_with_progress(&mut reader, &mut remote, total, progress)
            .with_context(|| format!("upload to {target} failed"))?;
        remote.close().ok();
        Ok(n)
    }

    /// Download `remote_name` (relative to the cwd or absolute) into `local`.
    pub fn download(&mut self, remote_name: &str, local: &Path, progress: &mut ProgressFn<'_>) -> Result<u64> {
        let source = join_remote(&self.cwd, remote_name);
        let mut remote =
            self.sftp.open(Path::new(&source)).with_context(|| format!("cannot open remote file {source}"))?;
        let total = remote.stat().ok().and_then(|s| s.size);
        let file = File::create(local).with_context(|| format!("cannot create {}", local.display()))?;
        let mut writer = BufWriter::new(file);
        let res = copy_with_progress(&mut remote, &mut writer, total, progress)
            .with_context(|| format!("download of {source} failed"));
        if res.is_err() {
            drop(writer);
            let _ = std::fs::remove_file(local);
        }
        res
    }

    /// Create a directory (relative to the cwd or absolute).
    pub fn mkdir(&mut self, name: &str) -> Result<()> {
        let target = join_remote(&self.cwd, name);
        self.sftp.mkdir(Path::new(&target), 0o755).with_context(|| format!("cannot create directory {target}"))
    }

    /// Delete a file, or an (empty) directory if `is_dir`.
    pub fn delete(&mut self, name: &str, is_dir: bool) -> Result<()> {
        let target = join_remote(&self.cwd, name);
        if is_dir {
            self.sftp
                .rmdir(Path::new(&target))
                .with_context(|| format!("cannot remove directory {target} (is it empty?)"))
        } else {
            self.sftp.unlink(Path::new(&target)).with_context(|| format!("cannot delete {target}"))
        }
    }

    /// Run `command` on the server (in a non-interactive shell) and collect its output.
    pub fn exec(&mut self, command: &str) -> Result<CommandOutput> {
        if command.trim().is_empty() {
            bail!("command is empty");
        }
        let mut channel = self.session.channel_session().context("cannot open SSH channel")?;
        channel.exec(command).with_context(|| format!("cannot execute `{command}`"))?;

        // Read stdout and stderr concurrently (non-blocking) so a chatty stderr
        // cannot dead-lock the channel while we wait for stdout.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        self.session.set_blocking(false);
        let result = (|| -> Result<()> {
            let mut buf = [0u8; 16 * 1024];
            let mut err_stream = channel.stderr();
            loop {
                let mut progressed = false;
                match channel.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        stdout.extend_from_slice(&buf[..n]);
                        progressed = true;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(anyhow!(e).context("reading stdout failed")),
                }
                match err_stream.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => {
                        stderr.extend_from_slice(&buf[..n]);
                        progressed = true;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(anyhow!(e).context("reading stderr failed")),
                }
                if !progressed {
                    if channel.eof() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Ok(())
        })();
        self.session.set_blocking(true);
        result?;
        channel.wait_close().context("closing channel failed")?;
        let exit_code = channel.exit_status().context("cannot read exit status")?;
        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            exit_code,
        })
    }

    /// Close the session.
    pub fn disconnect(self) -> Result<()> {
        drop(self.sftp);
        self.session.disconnect(None, "bye", None).context("disconnect failed")
    }
}

fn to_remote_string(p: &Path) -> String {
    // Remote paths are always '/'-separated, even when the client runs on Windows.
    let s = p.to_string_lossy().replace('\\', "/");
    if s.is_empty() { "/".into() } else { s }
}

/// Standard base64 without padding (OpenSSH fingerprint format).
fn base64_nopad(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64_nopad(b""), "");
        assert_eq!(base64_nopad(b"f"), "Zg");
        assert_eq!(base64_nopad(b"fo"), "Zm8");
        assert_eq!(base64_nopad(b"foo"), "Zm9v");
        assert_eq!(base64_nopad(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn debug_redacts_secrets() {
        let c = SshConfig {
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth: SshAuth::Password("s3cret".into()),
            timeout: Duration::from_secs(1),
        };
        assert!(!format!("{c:?}").contains("s3cret"));
    }

    #[test]
    fn remote_path_normalisation() {
        assert_eq!(to_remote_string(Path::new("")), "/");
        assert_eq!(to_remote_string(Path::new("/home/u")), "/home/u");
    }
}
