//! SSH back-end: SFTP file transfer and remote command execution via [`ssh2`] (libssh2).

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ssh2::{ErrorCode, HashType, KeyboardInteractivePrompt, MethodType, Prompt, Session, Sftp};

use crate::common::{ProgressFn, RemoteEntry, connect_tcp, copy_with_progress, join_remote};

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

/// Receives human readable progress/diagnostic lines while connecting.
pub type ConnectLog<'a> = dyn FnMut(String) + 'a;

/// An authenticated SSH session, with an SFTP channel when the server provides one.
pub struct SshClient {
    session: Session,
    /// `None` if the server has no `sftp` subsystem (e.g. some routers / dropbear builds):
    /// remote commands still work, file browsing does not.
    sftp: Option<Sftp>,
    cwd: String,
    fingerprint: String,
}

impl SshClient {
    /// Connect, perform the handshake, authenticate and open SFTP.
    ///
    /// Note: the server host key is *not* checked against `known_hosts`; its
    /// SHA-256 fingerprint is available via [`SshClient::fingerprint`] so the UI can show it.
    pub fn connect(cfg: &SshConfig) -> Result<Self> {
        Self::connect_with_log(cfg, &mut |_| {})
    }

    /// Like [`SshClient::connect`], reporting every stage (TCP peer, server version,
    /// negotiated algorithms, offered auth methods, …) to `log`.
    ///
    /// Errors start with the failing stage (`DNS:`, `TCP:`, `SSH handshake:`, `Auth:`)
    /// and include the libssh2 error code and message.
    pub fn connect_with_log(cfg: &SshConfig, log: &mut ConnectLog<'_>) -> Result<Self> {
        let user = cfg.username.trim();
        if user.is_empty() {
            bail!("username is empty");
        }
        if let SshAuth::KeyFile { path, .. } = &cfg.auth {
            check_key_file(path)?;
        }

        let tcp = connect_tcp(&cfg.host, cfg.port, cfg.timeout)?;
        if let Ok(peer) = tcp.peer_addr() {
            log(format!("TCP connected to {peer}"));
        }
        let mut session = Session::new().map_err(|e| anyhow!("cannot create SSH session: {}", ssh_error(&e)))?;
        session.set_tcp_stream(tcp);
        session.set_timeout(cfg.timeout.as_millis().clamp(1, u32::MAX as u128) as u32);
        if let Err(e) = session.handshake() {
            bail!("{}", handshake_error(&session, &e));
        }
        log(format!("Server software: {}", session.banner().unwrap_or("(unknown)")));
        log(negotiated_algorithms(&session));

        let fingerprint = session
            .host_key_hash(HashType::Sha256)
            .map(|h| format!("SHA256:{}", base64_nopad(h)))
            .unwrap_or_else(|| "unknown".into());

        authenticate(&session, user, &cfg.auth, log)?;

        // Transfers can be long; individual blocking calls still time out.
        session.set_timeout(0);
        session.set_keepalive(true, 30);
        let sftp = match session.sftp() {
            Ok(sftp) => Some(sftp),
            Err(e) => {
                log(format!(
                    "SFTP: subsystem not available ({}) – the file browser is disabled, remote commands still work",
                    ssh_error(&e)
                ));
                None
            }
        };
        let cwd = sftp
            .as_ref()
            .and_then(|s| s.realpath(Path::new(".")).ok())
            .map(|p| to_remote_string(&p))
            .unwrap_or_else(|| "/".into());
        Ok(Self { session, sftp, cwd, fingerprint })
    }

    /// Whether the SFTP subsystem is available (file browsing / transfers).
    pub fn has_sftp(&self) -> bool {
        self.sftp.is_some()
    }

    fn sftp(&self) -> Result<&Sftp> {
        self.sftp.as_ref().ok_or_else(|| {
            anyhow!("SFTP is not available on this server (no sftp subsystem); only remote commands work")
        })
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
        let real = self.sftp()?.realpath(Path::new(&target)).with_context(|| format!("cannot resolve {target}"))?;
        let stat = self.sftp()?.stat(&real).with_context(|| format!("cannot stat {}", real.display()))?;
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
        let sftp = self.sftp()?;
        let items = sftp.readdir(Path::new(&self.cwd)).with_context(|| format!("cannot list {}", self.cwd))?;
        let mut entries: Vec<RemoteEntry> = items
            .into_iter()
            .filter_map(|(path, stat)| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                if name == "." || name == ".." {
                    return None;
                }
                let is_dir = if stat.file_type().is_symlink() {
                    // Follow symlinks to decide whether they can be entered.
                    sftp.stat(&path).map(|s| s.is_dir()).unwrap_or(false)
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
            self.sftp()?.create(Path::new(&target)).with_context(|| format!("cannot create remote file {target}"))?;
        let n = copy_with_progress(&mut reader, &mut remote, total, progress)
            .with_context(|| format!("upload to {target} failed"))?;
        remote.close().ok();
        Ok(n)
    }

    /// Download `remote_name` (relative to the cwd or absolute) into `local`.
    pub fn download(&mut self, remote_name: &str, local: &Path, progress: &mut ProgressFn<'_>) -> Result<u64> {
        let source = join_remote(&self.cwd, remote_name);
        let mut remote =
            self.sftp()?.open(Path::new(&source)).with_context(|| format!("cannot open remote file {source}"))?;
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
        self.sftp()?.mkdir(Path::new(&target), 0o755).with_context(|| format!("cannot create directory {target}"))
    }

    /// Delete a file, or an (empty) directory if `is_dir`.
    pub fn delete(&mut self, name: &str, is_dir: bool) -> Result<()> {
        let target = join_remote(&self.cwd, name);
        if is_dir {
            self.sftp()?
                .rmdir(Path::new(&target))
                .with_context(|| format!("cannot remove directory {target} (is it empty?)"))
        } else {
            self.sftp()?.unlink(Path::new(&target)).with_context(|| format!("cannot delete {target}"))
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

/// Check a private key path before connecting, catching common mix-ups early.
fn check_key_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("Auth: private key file not found: {}", path.display());
    }
    let mut head = [0u8; 64];
    let n = File::open(path).and_then(|mut f| f.read(&mut head)).unwrap_or(0);
    let head = String::from_utf8_lossy(&head[..n]);
    if head.starts_with("PuTTY-User-Key-File") {
        bail!(
            "Auth: {} is a PuTTY .ppk key, which libssh2 cannot read. In PuTTYgen use \
             Conversions → Export OpenSSH key and select the exported file.",
            path.display()
        );
    }
    if head.starts_with("ssh-") || head.starts_with("ecdsa-") || head.starts_with("---- BEGIN SSH2 PUBLIC KEY") {
        bail!("Auth: {} is a public key; select the private key file (usually the one without .pub)", path.display());
    }
    Ok(())
}

/// Log line describing the algorithms negotiated during the handshake.
fn negotiated_algorithms(session: &Session) -> String {
    let m = |t| session.methods(t).unwrap_or("?");
    let cipher = m(MethodType::CryptCs);
    // AEAD ciphers authenticate by themselves; the negotiated MAC name is unused then.
    let mac = if cipher.contains("gcm") || cipher.contains("poly1305") {
        "implicit (AEAD)".to_string()
    } else {
        m(MethodType::MacCs).to_string()
    };
    format!("Negotiated: kex {}, host key {}, cipher {cipher}, mac {mac}", m(MethodType::Kex), m(MethodType::HostKey))
}

/// Describe a failed handshake, with hints and (for algorithm mismatches) what we support.
fn handshake_error(session: &Session, e: &ssh2::Error) -> String {
    let mut msg = format!("SSH handshake: {}", ssh_error(e));
    let code = match e.code() {
        ErrorCode::Session(c) => c,
        ErrorCode::SFTP(_) => 0,
    };
    match code {
        // KEX_FAILURE, KEY_EXCHANGE_FAILURE, HOSTKEY_INIT, METHOD_NOT_SUPPORTED, ALGO_UNSUPPORTED
        -5 | -8 | -10 | -33 | -51 => {
            let algs = |t| {
                let list = session.supported_algs(t).unwrap_or_default();
                // Drop protocol extension markers such as ext-info-c / kex-strict-c-v00@openssh.com.
                list.into_iter()
                    .filter(|a| !a.starts_with("ext-info") && !a.starts_with("kex-strict"))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            msg.push_str(&format!(
                " – no common algorithm with the server? This client supports kex [{}], host keys [{}], ciphers [{}]",
                algs(MethodType::Kex),
                algs(MethodType::HostKey),
                algs(MethodType::CryptCs)
            ));
        }
        // BANNER_RECV, SOCKET_DISCONNECT, SOCKET_RECV
        -2 | -13 | -43 => msg.push_str(
            " – the server closed the connection before the SSH handshake finished. Check that this port \
             really is an SSH server; the server may also be refusing this client temporarily (OpenSSH \
             PerSourcePenalties / MaxStartups, fail2ban) or by policy (AllowUsers, hosts.deny).",
        ),
        // TIMEOUT, SOCKET_TIMEOUT
        -9 | -30 => msg.push_str(" – the server did not answer in time (is this an SSH port?)"),
        _ => {}
    }
    msg
}

/// Authenticate `user`, choosing a method the server actually offers.
///
/// Password logins use the `password` method when offered and fall back to
/// `keyboard-interactive` (answering the prompts with the password). Many servers
/// only offer the latter, e.g. OpenSSH with `PasswordAuthentication no` +
/// `KbdInteractiveAuthentication yes` (PAM).
fn authenticate(session: &Session, user: &str, auth: &SshAuth, log: &mut ConnectLog<'_>) -> Result<()> {
    let offered = match session.auth_methods(user) {
        Ok(m) => m.to_string(),
        Err(e) => bail!("Auth: cannot get the list of authentication methods: {}", ssh_error(&e)),
    };
    if session.authenticated() {
        log("Auth: server accepted the login without credentials (\"none\" method)".into());
        return Ok(());
    }
    let shown = if offered.is_empty() { "(none)".to_string() } else { offered.clone() };
    log(format!("Server offers authentication methods: {shown}"));
    let has = |m: &str| offered.split(',').any(|x| x.trim() == m);

    match auth {
        SshAuth::Password(pw) => {
            let mut failures = Vec::new();
            if has("password") {
                match session.userauth_password(user, pw) {
                    Ok(()) if session.authenticated() => {
                        log("Authenticated with method \"password\"".into());
                        return Ok(());
                    }
                    Ok(()) => failures.push("password: not accepted".to_string()),
                    Err(e) => failures.push(format!("password: {}", ssh_error(&e))),
                }
            }
            if has("keyboard-interactive") {
                log(if failures.is_empty() {
                    "Server does not offer \"password\"; using \"keyboard-interactive\" with the password".into()
                } else {
                    "\"password\" was rejected; retrying with \"keyboard-interactive\"".into()
                });
                let mut prompter = PasswordPrompter::new(pw);
                let res = session.userauth_keyboard_interactive(user, &mut prompter);
                if !prompter.prompts.is_empty() {
                    log(format!("keyboard-interactive prompts answered: {}", prompter.prompts.join(" | ")));
                }
                match res {
                    Ok(()) if session.authenticated() => {
                        log("Authenticated with method \"keyboard-interactive\"".into());
                        return Ok(());
                    }
                    Ok(()) => failures.push("keyboard-interactive: not accepted".to_string()),
                    Err(e) => failures.push(format!("keyboard-interactive: {}", ssh_error(&e))),
                }
            }
            if failures.is_empty() {
                bail!(
                    "Auth: the server does not allow password logins (it offers: {shown}). Use a private key, \
                     or enable PasswordAuthentication / KbdInteractiveAuthentication in the server's sshd_config."
                );
            }
            bail!(
                "Auth: login as \"{user}\" was rejected – wrong user name or password? (server offers: {shown}; {})",
                failures.join("; ")
            )
        }
        SshAuth::KeyFile { path, passphrase } => {
            if !has("publickey") {
                bail!("Auth: the server does not allow public-key logins (it offers: {shown})");
            }
            let pass = passphrase.as_deref().filter(|p| !p.is_empty());
            match session.userauth_pubkey_file(user, None, path, pass) {
                Ok(()) if session.authenticated() => {
                    log(format!("Authenticated with method \"publickey\" ({})", path.display()));
                    Ok(())
                }
                Ok(()) => bail!("Auth: public key {} was not accepted", path.display()),
                Err(e) => {
                    let hint = match e.code() {
                        // FILE, KEYFILE_AUTH_FAILED
                        ErrorCode::Session(-16 | -48) => {
                            " – cannot load the key: wrong passphrase or unsupported key format"
                        }
                        // AUTHENTICATION_FAILED, PUBLICKEY_UNVERIFIED
                        ErrorCode::Session(-18 | -19) => {
                            " – the server rejected this key (is its .pub in ~/.ssh/authorized_keys of that user?)"
                        }
                        _ => "",
                    };
                    bail!("Auth: public key login with {} failed: {}{hint}", path.display(), ssh_error(&e))
                }
            }
        }
    }
}

/// Answers keyboard-interactive prompts with the password (like OpenSSH's client
/// does for the usual single `Password:` prompt) and remembers what was asked.
struct PasswordPrompter<'a> {
    password: &'a str,
    rounds: u32,
    prompts: Vec<String>,
}

impl<'a> PasswordPrompter<'a> {
    fn new(password: &'a str) -> Self {
        Self { password, rounds: 0, prompts: Vec::new() }
    }
}

impl KeyboardInteractivePrompt for PasswordPrompter<'_> {
    fn prompt<'b>(&mut self, _username: &str, _instructions: &str, prompts: &[Prompt<'b>]) -> Vec<String> {
        self.rounds += 1;
        for p in prompts {
            self.prompts.push(format!("{:?}", p.text.trim()));
        }
        // Guard against servers that keep asking (e.g. OTP challenges we cannot answer).
        let answer = if self.rounds > 3 { "" } else { self.password };
        prompts.iter().map(|_| answer.to_string()).collect()
    }
}

/// Format a libssh2 error as `message (libssh2 error NAME/-N)`.
pub(crate) fn ssh_error(e: &ssh2::Error) -> String {
    match e.code() {
        ErrorCode::Session(c) => format!("{} (libssh2 error {}/{c})", e.message(), libssh2_error_name(c)),
        ErrorCode::SFTP(c) => format!("{} (SFTP status {c})", e.message()),
    }
}

fn libssh2_error_name(code: i32) -> &'static str {
    const NAMES: [&str; 55] = [
        "NONE",
        "SOCKET_NONE",
        "BANNER_RECV",
        "BANNER_SEND",
        "INVALID_MAC",
        "KEX_FAILURE",
        "ALLOC",
        "SOCKET_SEND",
        "KEY_EXCHANGE_FAILURE",
        "TIMEOUT",
        "HOSTKEY_INIT",
        "HOSTKEY_SIGN",
        "DECRYPT",
        "SOCKET_DISCONNECT",
        "PROTO",
        "PASSWORD_EXPIRED",
        "FILE",
        "METHOD_NONE",
        "AUTHENTICATION_FAILED",
        "PUBLICKEY_UNVERIFIED",
        "CHANNEL_OUTOFORDER",
        "CHANNEL_FAILURE",
        "CHANNEL_REQUEST_DENIED",
        "CHANNEL_UNKNOWN",
        "CHANNEL_WINDOW_EXCEEDED",
        "CHANNEL_PACKET_EXCEEDED",
        "CHANNEL_CLOSED",
        "CHANNEL_EOF_SENT",
        "SCP_PROTOCOL",
        "ZLIB",
        "SOCKET_TIMEOUT",
        "SFTP_PROTOCOL",
        "REQUEST_DENIED",
        "METHOD_NOT_SUPPORTED",
        "INVAL",
        "INVALID_POLL_TYPE",
        "PUBLICKEY_PROTOCOL",
        "EAGAIN",
        "BUFFER_TOO_SMALL",
        "BAD_USE",
        "COMPRESS",
        "OUT_OF_BOUNDARY",
        "AGENT_PROTOCOL",
        "SOCKET_RECV",
        "ENCRYPT",
        "BAD_SOCKET",
        "KNOWN_HOSTS",
        "CHANNEL_WINDOW_FULL",
        "KEYFILE_AUTH_FAILED",
        "RANDGEN",
        "MISSING_USERAUTH_BANNER",
        "ALGO_UNSUPPORTED",
        "MAC_FAILURE",
        "HASH_INIT",
        "HASH_CALC",
    ];
    usize::try_from(-i64::from(code)).ok().and_then(|i| NAMES.get(i)).copied().unwrap_or("UNKNOWN")
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
    fn libssh2_error_names() {
        assert_eq!(libssh2_error_name(-18), "AUTHENTICATION_FAILED");
        assert_eq!(libssh2_error_name(-5), "KEX_FAILURE");
        assert_eq!(libssh2_error_name(-54), "HASH_CALC");
        assert_eq!(libssh2_error_name(-99), "UNKNOWN");
        assert_eq!(libssh2_error_name(3), "UNKNOWN");
    }

    #[test]
    fn prompter_answers_every_prompt_with_the_password() {
        let mut p = PasswordPrompter::new("pw");
        let prompts = [Prompt { text: "Password: ".into(), echo: false }, Prompt { text: "PIN".into(), echo: true }];
        assert_eq!(p.prompt("u", "", &prompts), vec!["pw".to_string(), "pw".to_string()]);
        assert_eq!(p.prompt("u", "", &[]), Vec::<String>::new());
        assert_eq!(p.prompts, vec!["\"Password:\"".to_string(), "\"PIN\"".to_string()]);
        p.prompt("u", "", &prompts);
        assert_eq!(p.prompt("u", "", &prompts), vec![String::new(), String::new()]);
    }

    #[test]
    fn key_file_mixups_are_reported_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let ppk = dir.path().join("k.ppk");
        std::fs::write(&ppk, "PuTTY-User-Key-File-3: ssh-ed25519\n").unwrap();
        assert!(check_key_file(&ppk).unwrap_err().to_string().contains("PuTTY"));
        let public = dir.path().join("k.pub");
        std::fs::write(&public, "ssh-ed25519 AAAA test\n").unwrap();
        assert!(check_key_file(&public).unwrap_err().to_string().contains("public key"));
        assert!(check_key_file(&dir.path().join("missing")).unwrap_err().to_string().contains("not found"));
        let ok = dir.path().join("id");
        std::fs::write(&ok, "-----BEGIN OPENSSH PRIVATE KEY-----\n").unwrap();
        assert!(check_key_file(&ok).is_ok());
    }

    #[test]
    fn remote_path_normalisation() {
        assert_eq!(to_remote_string(Path::new("")), "/");
        assert_eq!(to_remote_string(Path::new("/home/u")), "/home/u");
    }
}
