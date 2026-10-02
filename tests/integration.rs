//! End-to-end smoke tests of the protocol back-ends against real servers.
//!
//! These are `#[ignore]`d because they need running servers. Configure them with
//! environment variables and run `cargo test --test integration -- --ignored`:
//!
//! | variable                  | example                              |
//! |---------------------------|--------------------------------------|
//! | `RTG_FTP`                 | `127.0.0.1:2121:tester:s3cret`       |
//! | `RTG_SSH`                 | `127.0.0.1:2222:tester:s3cret`       |
//! | `RTG_SSH_KEY`             | `/path/to/id_ed25519` (optional)     |
//! | `RTG_SSH_KEY_PASSPHRASE`  | `keypass` (optional)                 |
//! | `RTG_SSH_KEY2`            | second key without passphrase (opt.) |
//! | `RTG_TFTP`                | `127.0.0.1:6969`                     |
//!
//! See `scripts/test-servers/` for small Python servers that work with these tests.

use std::path::PathBuf;
use std::time::Duration;

use rust_transfer_gui::ftp::{FtpClient, FtpConfig};
use rust_transfer_gui::sftp::{SshAuth, SshClient, SshConfig};
use rust_transfer_gui::tftp::{self, TftpConfig, TftpError};

fn env_parts(name: &str, n: usize) -> Vec<String> {
    let v = std::env::var(name).unwrap_or_else(|_| panic!("set {name} (see tests/integration.rs)"));
    let parts: Vec<String> = v.splitn(n, ':').map(str::to_string).collect();
    assert_eq!(parts.len(), n, "{name} must have {n} ':'-separated fields");
    parts
}

fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    // xorshift64*: deterministic, no extra dependency.
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 56) as u8
        })
        .collect()
}

fn unique(prefix: &str) -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("{prefix}-{t}")
}

// ------------------------------------------------------------------ FTP

fn ftp_cfg(passive: bool) -> FtpConfig {
    let p = env_parts("RTG_FTP", 4);
    FtpConfig {
        host: p[0].clone(),
        port: p[1].parse().unwrap(),
        username: p[2].clone(),
        password: p[3].clone(),
        passive,
        timeout: Duration::from_secs(10),
    }
}

fn ftp_roundtrip(passive: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let data = random_bytes(345_678, 1);
    let local = tmp.path().join("up.bin");
    std::fs::write(&local, &data).unwrap();

    let mut c = FtpClient::connect(&ftp_cfg(passive)).expect("connect");
    let start = c.pwd().unwrap();
    let dir = unique("rtg-ftp");
    c.mkdir(&dir).unwrap();
    c.cwd(&dir).unwrap();
    let mut last = 0;
    let n = c.upload(&local, "remote.bin", &mut |d, _| last = d).unwrap();
    assert_eq!(n as usize, data.len());
    assert_eq!(last as usize, data.len());

    let list = c.list().unwrap();
    let e = list.iter().find(|e| e.name == "remote.bin").expect("uploaded file listed");
    assert_eq!(e.size, Some(data.len() as u64));
    assert!(!e.is_dir);

    let down = tmp.path().join("down.bin");
    let n = c.download("remote.bin", &down, &mut |_, _| {}).unwrap();
    assert_eq!(n as usize, data.len());
    assert_eq!(std::fs::read(&down).unwrap(), data);

    // Missing file: error, no panic, no partial file left behind.
    let missing = tmp.path().join("missing.bin");
    assert!(c.download("does-not-exist.bin", &missing, &mut |_, _| {}).is_err());
    assert!(!missing.exists());

    c.delete("remote.bin", false).unwrap();
    c.cdup().unwrap();
    assert_eq!(c.pwd().unwrap(), start);
    let list = c.list().unwrap();
    assert!(list.iter().any(|e| e.name == dir && e.is_dir));
    c.delete(&dir, true).unwrap();
    c.quit().unwrap();
}

#[test]
#[ignore = "needs an FTP server (RTG_FTP)"]
fn ftp_passive_roundtrip() {
    ftp_roundtrip(true);
}

#[test]
#[ignore = "needs an FTP server (RTG_FTP)"]
fn ftp_active_roundtrip() {
    ftp_roundtrip(false);
}

#[test]
#[ignore = "needs an FTP server (RTG_FTP)"]
fn ftp_bad_password_is_an_error() {
    let mut cfg = ftp_cfg(true);
    cfg.password.push_str("-wrong");
    assert!(FtpClient::connect(&cfg).is_err());
}

// ------------------------------------------------------------------ SSH / SFTP

fn ssh_cfg(auth: Option<SshAuth>) -> SshConfig {
    let p = env_parts("RTG_SSH", 4);
    SshConfig {
        host: p[0].clone(),
        port: p[1].parse().unwrap(),
        username: p[2].clone(),
        auth: auth.unwrap_or_else(|| SshAuth::Password(p[3].clone())),
        timeout: Duration::from_secs(10),
    }
}

#[test]
#[ignore = "needs an SSH server (RTG_SSH)"]
fn ssh_sftp_roundtrip_and_exec() {
    let tmp = tempfile::tempdir().unwrap();
    let data = random_bytes(1_234_567, 2);
    let local = tmp.path().join("up.bin");
    std::fs::write(&local, &data).unwrap();

    let mut c = SshClient::connect(&ssh_cfg(None)).expect("connect");
    assert!(c.fingerprint().starts_with("SHA256:"));
    let home = c.cwd().to_string();
    let dir = unique("rtg-sftp");
    c.mkdir(&dir).unwrap();
    c.cd(&dir).unwrap();
    assert!(c.cwd().ends_with(&dir));

    let n = c.upload(&local, "remote.bin", &mut |_, _| {}).unwrap();
    assert_eq!(n as usize, data.len());
    let list = c.list().unwrap();
    let e = list.iter().find(|e| e.name == "remote.bin").expect("listed");
    assert_eq!(e.size, Some(data.len() as u64));
    assert!(e.modified.is_some());

    let down = tmp.path().join("down.bin");
    let mut total_seen = None;
    c.download("remote.bin", &down, &mut |_, t| total_seen = t).unwrap();
    assert_eq!(total_seen, Some(data.len() as u64));
    assert_eq!(std::fs::read(&down).unwrap(), data);

    assert!(c.cd("remote.bin").is_err(), "cd into a file must fail");
    c.delete("remote.bin", false).unwrap();
    c.up().unwrap();
    assert_eq!(c.cwd(), home);
    c.delete(&dir, true).unwrap();

    let out = c.exec("echo hello; echo oops >&2; exit 3").unwrap();
    assert_eq!(out.stdout, "hello\n");
    assert_eq!(out.stderr, "oops\n");
    assert_eq!(out.exit_code, 3);

    // Lots of stderr and stdout at once must not dead-lock.
    let out = c.exec("head -c 400000 /dev/zero | tr '\\0' e >&2; head -c 300000 /dev/zero | tr '\\0' o; echo").unwrap();
    assert_eq!(out.stderr.len(), 400_000);
    assert_eq!(out.stdout.len(), 300_001);
    assert_eq!(out.exit_code, 0);

    c.disconnect().unwrap();
}

#[test]
#[ignore = "needs an SSH server (RTG_SSH)"]
fn ssh_wrong_password_is_an_error() {
    let p = env_parts("RTG_SSH", 4);
    let cfg = ssh_cfg(Some(SshAuth::Password(format!("{}-wrong", p[3]))));
    assert!(SshClient::connect(&cfg).is_err());
}

#[test]
#[ignore = "needs an SSH server and RTG_SSH_KEY"]
fn ssh_key_auth() {
    let key = PathBuf::from(std::env::var("RTG_SSH_KEY").expect("set RTG_SSH_KEY"));
    let passphrase = std::env::var("RTG_SSH_KEY_PASSPHRASE").ok();
    let mut c =
        SshClient::connect(&ssh_cfg(Some(SshAuth::KeyFile { path: key.clone(), passphrase: passphrase.clone() })))
            .expect("key auth");
    assert_eq!(c.exec("echo key-ok").unwrap().stdout, "key-ok\n");
    c.disconnect().unwrap();

    if passphrase.is_some() {
        let bad = SshAuth::KeyFile { path: key, passphrase: Some("definitely-wrong".into()) };
        assert!(SshClient::connect(&ssh_cfg(Some(bad))).is_err());
    }
    if let Ok(k2) = std::env::var("RTG_SSH_KEY2") {
        let mut c = SshClient::connect(&ssh_cfg(Some(SshAuth::KeyFile { path: k2.into(), passphrase: None })))
            .expect("second key auth");
        assert_eq!(c.exec("echo key2-ok").unwrap().stdout, "key2-ok\n");
        c.disconnect().unwrap();
    }
}

// ------------------------------------------------------------------ TFTP

fn tftp_cfg() -> TftpConfig {
    let p = env_parts("RTG_TFTP", 2);
    TftpConfig { host: p[0].clone(), port: p[1].parse().unwrap(), timeout: Duration::from_secs(2), retries: 5 }
}

#[test]
#[ignore = "needs a TFTP server (RTG_TFTP)"]
fn tftp_put_then_get() {
    let tmp = tempfile::tempdir().unwrap();
    for (i, len) in [0usize, 512, 1024, 123_457].into_iter().enumerate() {
        let data = random_bytes(len, 10 + i as u64);
        let local = tmp.path().join(format!("up{i}.bin"));
        std::fs::write(&local, &data).unwrap();
        let name = unique("rtg-tftp");
        let sent = tftp::put_file(&tftp_cfg(), &local, &name, &mut |_, _| {}).unwrap();
        assert_eq!(sent as usize, len);
        let down = tmp.path().join(format!("down{i}.bin"));
        let got = tftp::get_file(&tftp_cfg(), &name, &down, &mut |_, _| {}).unwrap();
        assert_eq!(got as usize, len);
        assert_eq!(std::fs::read(&down).unwrap(), data, "len {len}");
    }
}

#[test]
#[ignore = "needs a TFTP server (RTG_TFTP)"]
fn tftp_missing_file_is_a_remote_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("x");
    let err = tftp::get_file(&tftp_cfg(), "no-such-file-here.bin", &dest, &mut |_, _| {}).unwrap_err();
    assert!(matches!(err, TftpError::Remote { code: 1, .. }), "{err:?}");
    assert!(!dest.exists(), "partial file must be removed");
}
