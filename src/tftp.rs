//! A small TFTP client (RFC 1350) built directly on [`std::net::UdpSocket`].
//!
//! Supported:
//! * read requests (RRQ / "get") and write requests (WRQ / "put"),
//! * `octet` transfer mode with the classic 512-byte block size,
//! * `ERROR` packets in both directions,
//! * timeouts with a configurable number of retransmissions,
//! * transfer-ID (TID) checking: packets from unexpected ports are answered with
//!   error 5 ("Unknown transfer ID") and otherwise ignored,
//! * block-number wraparound (65535 -> 0) so files larger than 32 MiB work,
//! * protection against the "Sorcerer's Apprentice" bug (duplicate ACKs never
//!   trigger a retransmission of DATA).
//!
//! Option negotiation (RFC 2347, blksize/tsize/...) is intentionally not implemented.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::common::{ProgressFn, resolve};

/// Payload bytes per DATA packet (RFC 1350).
pub const BLOCK_SIZE: usize = 512;
/// Default TFTP server port.
pub const DEFAULT_PORT: u16 = 69;
/// The only transfer mode this client uses.
pub const MODE_OCTET: &str = "octet";

const OP_RRQ: u16 = 1;
const OP_WRQ: u16 = 2;
const OP_DATA: u16 = 3;
const OP_ACK: u16 = 4;
const OP_ERROR: u16 = 5;

/// TFTP error codes (RFC 1350, section 5 / appendix).
pub mod error_code {
    pub const NOT_DEFINED: u16 = 0;
    pub const FILE_NOT_FOUND: u16 = 1;
    pub const ACCESS_VIOLATION: u16 = 2;
    pub const DISK_FULL: u16 = 3;
    pub const ILLEGAL_OPERATION: u16 = 4;
    pub const UNKNOWN_TID: u16 = 5;
    pub const FILE_EXISTS: u16 = 6;
    pub const NO_SUCH_USER: u16 = 7;
}

/// A decoded TFTP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Rrq { filename: String, mode: String },
    Wrq { filename: String, mode: String },
    Data { block: u16, data: Vec<u8> },
    Ack { block: u16 },
    Error { code: u16, message: String },
}

impl Packet {
    /// Serialize the packet into its on-the-wire representation.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + BLOCK_SIZE);
        match self {
            Packet::Rrq { filename, mode } | Packet::Wrq { filename, mode } => {
                let op = if matches!(self, Packet::Rrq { .. }) { OP_RRQ } else { OP_WRQ };
                out.extend_from_slice(&op.to_be_bytes());
                out.extend_from_slice(filename.as_bytes());
                out.push(0);
                out.extend_from_slice(mode.as_bytes());
                out.push(0);
            }
            Packet::Data { block, data } => {
                out.extend_from_slice(&OP_DATA.to_be_bytes());
                out.extend_from_slice(&block.to_be_bytes());
                out.extend_from_slice(data);
            }
            Packet::Ack { block } => {
                out.extend_from_slice(&OP_ACK.to_be_bytes());
                out.extend_from_slice(&block.to_be_bytes());
            }
            Packet::Error { code, message } => {
                out.extend_from_slice(&OP_ERROR.to_be_bytes());
                out.extend_from_slice(&code.to_be_bytes());
                out.extend_from_slice(message.as_bytes());
                out.push(0);
            }
        }
        out
    }

    /// Parse a packet received from the network.
    pub fn decode(buf: &[u8]) -> Result<Packet, TftpError> {
        if buf.len() < 2 {
            return Err(TftpError::Malformed("packet shorter than 2 bytes".into()));
        }
        let opcode = u16::from_be_bytes([buf[0], buf[1]]);
        let body = &buf[2..];
        match opcode {
            OP_RRQ | OP_WRQ => {
                let mut parts = body.split(|&b| b == 0);
                let filename = parts.next().unwrap_or_default();
                let mode = parts.next();
                // A well-formed request ends with a NUL after the mode.
                let (Some(mode), true) = (mode, body.ends_with(&[0])) else {
                    return Err(TftpError::Malformed("request is not NUL-terminated".into()));
                };
                if filename.is_empty() {
                    return Err(TftpError::Malformed("empty filename".into()));
                }
                let filename = String::from_utf8_lossy(filename).into_owned();
                let mode = String::from_utf8_lossy(mode).to_ascii_lowercase();
                Ok(if opcode == OP_RRQ { Packet::Rrq { filename, mode } } else { Packet::Wrq { filename, mode } })
            }
            OP_DATA => {
                if body.len() < 2 {
                    return Err(TftpError::Malformed("DATA packet too short".into()));
                }
                Ok(Packet::Data { block: u16::from_be_bytes([body[0], body[1]]), data: body[2..].to_vec() })
            }
            OP_ACK => {
                if body.len() < 2 {
                    return Err(TftpError::Malformed("ACK packet too short".into()));
                }
                Ok(Packet::Ack { block: u16::from_be_bytes([body[0], body[1]]) })
            }
            OP_ERROR => {
                if body.len() < 2 {
                    return Err(TftpError::Malformed("ERROR packet too short".into()));
                }
                let code = u16::from_be_bytes([body[0], body[1]]);
                let msg = &body[2..];
                let msg = msg.split(|&b| b == 0).next().unwrap_or_default();
                Ok(Packet::Error { code, message: String::from_utf8_lossy(msg).into_owned() })
            }
            other => Err(TftpError::Malformed(format!("unknown opcode {other}"))),
        }
    }
}

/// Errors produced by the TFTP client.
#[derive(Debug)]
pub enum TftpError {
    Io(io::Error),
    /// A packet could not be parsed.
    Malformed(String),
    /// The server sent an ERROR packet.
    Remote {
        code: u16,
        message: String,
    },
    /// No answer after all retransmissions.
    Timeout {
        retries: u32,
    },
    /// The peer violated the protocol.
    Protocol(String),
    /// Address resolution or configuration problem.
    Config(String),
}

impl fmt::Display for TftpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TftpError::Io(e) => write!(f, "I/O error: {e}"),
            TftpError::Malformed(m) => write!(f, "malformed packet: {m}"),
            TftpError::Remote { code, message } => {
                write!(f, "server error {code} ({}): {message}", error_name(*code))
            }
            TftpError::Timeout { retries } => {
                write!(f, "timed out waiting for the server (after {retries} retransmissions)")
            }
            TftpError::Protocol(m) => write!(f, "protocol error: {m}"),
            TftpError::Config(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for TftpError {}

impl From<io::Error> for TftpError {
    fn from(e: io::Error) -> Self {
        TftpError::Io(e)
    }
}

/// Human readable name of a TFTP error code.
pub fn error_name(code: u16) -> &'static str {
    match code {
        error_code::NOT_DEFINED => "not defined",
        error_code::FILE_NOT_FOUND => "file not found",
        error_code::ACCESS_VIOLATION => "access violation",
        error_code::DISK_FULL => "disk full or allocation exceeded",
        error_code::ILLEGAL_OPERATION => "illegal TFTP operation",
        error_code::UNKNOWN_TID => "unknown transfer ID",
        error_code::FILE_EXISTS => "file already exists",
        error_code::NO_SUCH_USER => "no such user",
        _ => "unknown error code",
    }
}

/// Client configuration.
#[derive(Debug, Clone)]
pub struct TftpConfig {
    pub host: String,
    pub port: u16,
    /// How long to wait for each reply before retransmitting.
    pub timeout: Duration,
    /// How many retransmissions before giving up.
    pub retries: u32,
}

impl Default for TftpConfig {
    fn default() -> Self {
        Self { host: String::new(), port: DEFAULT_PORT, timeout: Duration::from_secs(3), retries: 5 }
    }
}

/// Internal helper wrapping the socket and the transfer ID logic.
struct Conn {
    sock: UdpSocket,
    server: SocketAddr,
    /// The server's transfer ID (address + port), learned from its first reply.
    peer: Option<SocketAddr>,
    buf: Vec<u8>,
}

enum Recv {
    Packet(Packet),
    Timeout,
}

impl Conn {
    fn open(cfg: &TftpConfig) -> Result<Self, TftpError> {
        let server = resolve(&cfg.host, cfg.port).map_err(|e| TftpError::Config(e.to_string()))?;
        let bind: SocketAddr = if server.is_ipv4() {
            "0.0.0.0:0".parse().expect("valid literal")
        } else {
            "[::]:0".parse().expect("valid literal")
        };
        let sock = UdpSocket::bind(bind)?;
        Ok(Self { sock, server, peer: None, buf: vec![0u8; 65536] })
    }

    /// Where packets should currently be sent.
    fn dest(&self) -> SocketAddr {
        self.peer.unwrap_or(self.server)
    }

    fn send(&self, pkt: &[u8]) -> Result<(), TftpError> {
        self.sock.send_to(pkt, self.dest())?;
        Ok(())
    }

    fn send_error(&self, code: u16, message: &str) {
        let pkt = Packet::Error { code, message: message.to_string() }.encode();
        // Best effort: errors are never acknowledged or retransmitted.
        let _ = self.sock.send_to(&pkt, self.dest());
    }

    /// Wait for the next valid packet from the server until `deadline`.
    fn recv(&mut self, deadline: Instant) -> Result<Recv, TftpError> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(Recv::Timeout);
            }
            self.sock.set_read_timeout(Some(deadline - now))?;
            let (n, from) = match self.sock.recv_from(&mut self.buf) {
                Ok(v) => v,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    return Ok(Recv::Timeout);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused) => {
                    // Windows reports ICMP "port unreachable" this way.
                    return Err(TftpError::Protocol(format!(
                        "server {} is unreachable (no TFTP server listening?): {e}",
                        self.server
                    )));
                }
                Err(e) => return Err(e.into()),
            };
            match self.peer {
                Some(p) if p != from => {
                    // Packet from a foreign TID: tell it off, keep waiting (RFC 1350 §4).
                    let pkt =
                        Packet::Error { code: error_code::UNKNOWN_TID, message: "Unknown transfer ID".into() }.encode();
                    let _ = self.sock.send_to(&pkt, from);
                    continue;
                }
                Some(_) => {}
                None => {
                    if canonical(from.ip()) != canonical(self.server.ip()) {
                        // Not from the host we talked to; ignore.
                        continue;
                    }
                }
            }
            let Ok(pkt) = Packet::decode(&self.buf[..n]) else {
                // Garbage is silently dropped.
                continue;
            };
            if self.peer.is_none() {
                self.peer = Some(from);
            }
            return Ok(Recv::Packet(pkt));
        }
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

/// Read until `buf` is full or EOF; returns the number of bytes read.
fn read_full<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Download `remote` from the server into `writer`. Returns the number of bytes received.
pub fn get<W: Write + ?Sized>(
    cfg: &TftpConfig,
    remote: &str,
    writer: &mut W,
    progress: &mut ProgressFn<'_>,
) -> Result<u64, TftpError> {
    if remote.is_empty() {
        return Err(TftpError::Config("remote filename is empty".into()));
    }
    let mut conn = Conn::open(cfg)?;
    let mut last = Packet::Rrq { filename: remote.to_string(), mode: MODE_OCTET.into() }.encode();
    conn.send(&last)?;
    let mut deadline = Instant::now() + cfg.timeout;
    let mut retries = 0u32;
    let mut expected: u16 = 1;
    let mut acked_any = false;
    let mut total: u64 = 0;
    progress(0, None);

    loop {
        match conn.recv(deadline)? {
            Recv::Timeout => {
                if retries >= cfg.retries {
                    return Err(TftpError::Timeout { retries });
                }
                retries += 1;
                conn.send(&last)?;
                deadline = Instant::now() + cfg.timeout;
            }
            Recv::Packet(Packet::Data { block, data }) => {
                if block == expected {
                    if data.len() > BLOCK_SIZE {
                        conn.send_error(error_code::ILLEGAL_OPERATION, "DATA block too large");
                        return Err(TftpError::Protocol(format!(
                            "DATA block {block} has {} bytes (max {BLOCK_SIZE})",
                            data.len()
                        )));
                    }
                    if let Err(e) = writer.write_all(&data) {
                        conn.send_error(error_code::DISK_FULL, "write failed on client");
                        return Err(e.into());
                    }
                    total += data.len() as u64;
                    progress(total, None);
                    last = Packet::Ack { block }.encode();
                    conn.send(&last)?;
                    acked_any = true;
                    retries = 0;
                    deadline = Instant::now() + cfg.timeout;
                    if data.len() < BLOCK_SIZE {
                        writer.flush()?;
                        return Ok(total);
                    }
                    expected = expected.wrapping_add(1);
                } else if acked_any && block == expected.wrapping_sub(1) {
                    // Our ACK got lost: acknowledge the duplicate again.
                    conn.send(&last)?;
                }
                // Anything else is an old duplicate: ignore.
            }
            Recv::Packet(Packet::Error { code, message }) => {
                return Err(TftpError::Remote { code, message });
            }
            Recv::Packet(other) => {
                conn.send_error(error_code::ILLEGAL_OPERATION, "unexpected packet");
                return Err(TftpError::Protocol(format!("unexpected packet during RRQ: {other:?}")));
            }
        }
    }
}

/// Upload everything from `reader` to the server as `remote`. Returns bytes sent.
pub fn put<R: Read + ?Sized>(
    cfg: &TftpConfig,
    remote: &str,
    reader: &mut R,
    total_size: Option<u64>,
    progress: &mut ProgressFn<'_>,
) -> Result<u64, TftpError> {
    if remote.is_empty() {
        return Err(TftpError::Config("remote filename is empty".into()));
    }
    let mut conn = Conn::open(cfg)?;
    let mut last = Packet::Wrq { filename: remote.to_string(), mode: MODE_OCTET.into() }.encode();
    conn.send(&last)?;
    let mut deadline = Instant::now() + cfg.timeout;
    let mut retries = 0u32;
    let mut awaiting: u16 = 0; // ACK 0 answers the WRQ
    let mut pending_len: usize = 0;
    let mut finished = false;
    let mut sent: u64 = 0;
    let mut chunk = vec![0u8; BLOCK_SIZE];
    progress(0, total_size);

    loop {
        match conn.recv(deadline)? {
            Recv::Timeout => {
                if retries >= cfg.retries {
                    return Err(TftpError::Timeout { retries });
                }
                retries += 1;
                conn.send(&last)?;
                deadline = Instant::now() + cfg.timeout;
            }
            Recv::Packet(Packet::Ack { block }) => {
                if block != awaiting {
                    // Duplicate/old ACK: never retransmit on it (Sorcerer's Apprentice).
                    continue;
                }
                sent += pending_len as u64;
                if awaiting != 0 || pending_len > 0 {
                    progress(sent, total_size);
                }
                if finished {
                    return Ok(sent);
                }
                let n = match read_full(reader, &mut chunk) {
                    Ok(n) => n,
                    Err(e) => {
                        conn.send_error(error_code::NOT_DEFINED, "read failed on client");
                        return Err(e.into());
                    }
                };
                awaiting = awaiting.wrapping_add(1);
                pending_len = n;
                finished = n < BLOCK_SIZE;
                last = Packet::Data { block: awaiting, data: chunk[..n].to_vec() }.encode();
                conn.send(&last)?;
                retries = 0;
                deadline = Instant::now() + cfg.timeout;
            }
            Recv::Packet(Packet::Error { code, message }) => {
                return Err(TftpError::Remote { code, message });
            }
            Recv::Packet(other) => {
                conn.send_error(error_code::ILLEGAL_OPERATION, "unexpected packet");
                return Err(TftpError::Protocol(format!("unexpected packet during WRQ: {other:?}")));
            }
        }
    }
}

/// Download `remote` into the local file `local`. A partial file is removed on failure.
pub fn get_file(cfg: &TftpConfig, remote: &str, local: &Path, progress: &mut ProgressFn<'_>) -> Result<u64, TftpError> {
    let file = File::create(local)?;
    let mut w = BufWriter::new(file);
    let res = get(cfg, remote, &mut w, progress).and_then(|n| {
        w.flush()?;
        Ok(n)
    });
    if res.is_err() {
        drop(w);
        let _ = std::fs::remove_file(local);
    }
    res
}

/// Upload the local file `local` to the server as `remote`.
pub fn put_file(cfg: &TftpConfig, local: &Path, remote: &str, progress: &mut ProgressFn<'_>) -> Result<u64, TftpError> {
    let file = File::open(local)?;
    let size = file.metadata().ok().map(|m| m.len());
    let mut r = BufReader::new(file);
    put(cfg, remote, &mut r, size, progress)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;

    // ---------- packet encoding / decoding ----------

    #[test]
    fn encode_rrq_exact_bytes() {
        let p = Packet::Rrq { filename: "a.txt".into(), mode: "octet".into() };
        assert_eq!(p.encode(), b"\x00\x01a.txt\x00octet\x00".to_vec());
    }

    #[test]
    fn encode_wrq_exact_bytes() {
        let p = Packet::Wrq { filename: "dir/b.bin".into(), mode: "octet".into() };
        assert_eq!(p.encode(), b"\x00\x02dir/b.bin\x00octet\x00".to_vec());
    }

    #[test]
    fn encode_data_ack_error_exact_bytes() {
        assert_eq!(Packet::Data { block: 0x0102, data: vec![9, 8, 7] }.encode(), vec![0, 3, 1, 2, 9, 8, 7]);
        assert_eq!(Packet::Ack { block: 65535 }.encode(), vec![0, 4, 0xff, 0xff]);
        assert_eq!(Packet::Error { code: 1, message: "nope".into() }.encode(), b"\x00\x05\x00\x01nope\x00".to_vec());
    }

    #[test]
    fn roundtrip_all_packet_types() {
        let packets = vec![
            Packet::Rrq { filename: "x".into(), mode: "octet".into() },
            Packet::Wrq { filename: "y/z".into(), mode: "netascii".into() },
            Packet::Data { block: 1, data: vec![] },
            Packet::Data { block: 65535, data: vec![0xAA; BLOCK_SIZE] },
            Packet::Ack { block: 0 },
            Packet::Ack { block: 4242 },
            Packet::Error { code: 5, message: "Unknown transfer ID".into() },
            Packet::Error { code: 0, message: String::new() },
        ];
        for p in packets {
            assert_eq!(Packet::decode(&p.encode()).unwrap(), p);
        }
    }

    #[test]
    fn decode_mode_is_case_insensitive() {
        let p = Packet::decode(b"\x00\x01f\x00OcTeT\x00").unwrap();
        assert_eq!(p, Packet::Rrq { filename: "f".into(), mode: "octet".into() });
    }

    #[test]
    fn decode_request_with_options_ignores_them() {
        let p = Packet::decode(b"\x00\x01f\x00octet\x00blksize\x001024\x00").unwrap();
        assert_eq!(p, Packet::Rrq { filename: "f".into(), mode: "octet".into() });
    }

    #[test]
    fn decode_error_without_nul() {
        let p = Packet::decode(b"\x00\x05\x00\x02denied").unwrap();
        assert_eq!(p, Packet::Error { code: 2, message: "denied".into() });
    }

    #[test]
    fn decode_rejects_malformed() {
        for bad in [
            &b""[..],
            b"\x00",
            b"\x00\x09\x00\x00",      // unknown opcode
            b"\x00\x03\x00",          // short DATA
            b"\x00\x04\x01",          // short ACK
            b"\x00\x05\x00",          // short ERROR
            b"\x00\x01file\x00octet", // missing final NUL
            b"\x00\x01file",          // missing mode
            b"\x00\x02\x00octet\x00", // empty filename
        ] {
            assert!(Packet::decode(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn block_number_wraps() {
        assert_eq!(65535u16.wrapping_add(1), 0);
        assert_eq!(0u16.wrapping_sub(1), 65535);
    }

    #[test]
    fn error_display_mentions_code() {
        let e = TftpError::Remote { code: 1, message: "missing.bin".into() };
        let s = e.to_string();
        assert!(s.contains("file not found") && s.contains("missing.bin"));
    }

    // ---------- loopback tests against a tiny in-process server ----------

    #[derive(Clone, Default)]
    struct ServerOpts {
        /// Drop the first copy of this DATA/ACK block number (simulate packet loss).
        drop_once: Option<u16>,
        /// Answer every request with this error.
        error: Option<(u16, String)>,
    }

    /// Serve exactly one request. RRQ serves `content`; WRQ stores into `stored`.
    fn spawn_server(content: Vec<u8>, stored: Arc<Mutex<Vec<u8>>>, opts: ServerOpts) -> (u16, thread::JoinHandle<()>) {
        let listen = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = listen.local_addr().unwrap().port();
        let h = thread::spawn(move || {
            let mut buf = vec![0u8; 2048];
            listen.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let (n, client) = listen.recv_from(&mut buf).unwrap();
            let req = Packet::decode(&buf[..n]).unwrap();
            // New socket = new server TID.
            let s = UdpSocket::bind("127.0.0.1:0").unwrap();
            s.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
            if let Some((code, message)) = opts.error.clone() {
                s.send_to(&Packet::Error { code, message }.encode(), client).unwrap();
                return;
            }
            let mut dropped = false;
            match req {
                Packet::Rrq { mode, .. } => {
                    assert_eq!(mode, "octet");
                    let mut block: u16 = 1;
                    let mut off = 0usize;
                    loop {
                        let end = (off + BLOCK_SIZE).min(content.len());
                        let pkt = Packet::Data { block, data: content[off..end].to_vec() }.encode();
                        let mut tries = 0;
                        loop {
                            if opts.drop_once == Some(block) && !dropped {
                                dropped = true;
                            } else {
                                s.send_to(&pkt, client).unwrap();
                            }
                            match s.recv_from(&mut buf) {
                                Ok((n, _)) => match Packet::decode(&buf[..n]).unwrap() {
                                    Packet::Ack { block: b } if b == block => break,
                                    _ => continue,
                                },
                                Err(_) => {
                                    tries += 1;
                                    assert!(tries < 20, "client stopped acking");
                                }
                            }
                        }
                        if end - off < BLOCK_SIZE {
                            return;
                        }
                        off = end;
                        block = block.wrapping_add(1);
                    }
                }
                Packet::Wrq { .. } => {
                    let mut expected: u16 = 1;
                    s.send_to(&Packet::Ack { block: 0 }.encode(), client).unwrap();
                    let mut last_ack = Packet::Ack { block: 0 }.encode();
                    loop {
                        match s.recv_from(&mut buf) {
                            Ok((n, _)) => {
                                if let Packet::Data { block, data } = Packet::decode(&buf[..n]).unwrap() {
                                    if block == expected {
                                        if opts.drop_once == Some(block) && !dropped {
                                            // Pretend we never got it.
                                            dropped = true;
                                            continue;
                                        }
                                        stored.lock().unwrap().extend_from_slice(&data);
                                        last_ack = Packet::Ack { block }.encode();
                                        s.send_to(&last_ack, client).unwrap();
                                        if data.len() < BLOCK_SIZE {
                                            return;
                                        }
                                        expected = expected.wrapping_add(1);
                                    } else {
                                        s.send_to(&last_ack, client).unwrap();
                                    }
                                }
                            }
                            Err(_) => s.send_to(&last_ack, client).map(|_| ()).unwrap(),
                        }
                    }
                }
                other => panic!("unexpected request {other:?}"),
            }
        });
        (port, h)
    }

    fn cfg(port: u16) -> TftpConfig {
        TftpConfig { host: "127.0.0.1".into(), port, timeout: Duration::from_millis(200), retries: 10 }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    #[test]
    fn loopback_get_various_sizes() {
        for len in [0usize, 1, 511, 512, 513, 1024, 5000] {
            let data = pattern(len);
            let (port, h) = spawn_server(data.clone(), Default::default(), Default::default());
            let mut out = Vec::new();
            let n = get(&cfg(port), "f.bin", &mut out, &mut |_, _| {}).unwrap();
            h.join().unwrap();
            assert_eq!(n as usize, len);
            assert_eq!(out, data, "len {len}");
        }
    }

    #[test]
    fn loopback_put_various_sizes() {
        for len in [0usize, 1, 511, 512, 513, 1024, 5000] {
            let data = pattern(len);
            let stored = Arc::new(Mutex::new(Vec::new()));
            let (port, h) = spawn_server(Vec::new(), stored.clone(), Default::default());
            let n = put(&cfg(port), "f.bin", &mut &data[..], Some(len as u64), &mut |_, _| {}).unwrap();
            h.join().unwrap();
            assert_eq!(n as usize, len);
            assert_eq!(*stored.lock().unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn loopback_get_recovers_from_lost_data() {
        let data = pattern(3000);
        let opts = ServerOpts { drop_once: Some(3), ..Default::default() };
        let (port, h) = spawn_server(data.clone(), Default::default(), opts);
        let mut out = Vec::new();
        get(&cfg(port), "f.bin", &mut out, &mut |_, _| {}).unwrap();
        h.join().unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn loopback_put_recovers_from_lost_data() {
        let data = pattern(3000);
        let stored = Arc::new(Mutex::new(Vec::new()));
        let opts = ServerOpts { drop_once: Some(2), ..Default::default() };
        let (port, h) = spawn_server(Vec::new(), stored.clone(), opts);
        put(&cfg(port), "f.bin", &mut &data[..], None, &mut |_, _| {}).unwrap();
        h.join().unwrap();
        assert_eq!(*stored.lock().unwrap(), data);
    }

    #[test]
    fn loopback_server_error_is_reported() {
        let opts = ServerOpts { error: Some((1, "File not found".into())), ..Default::default() };
        let (port, h) = spawn_server(Vec::new(), Default::default(), opts);
        let err = get(&cfg(port), "missing", &mut Vec::new(), &mut |_, _| {}).unwrap_err();
        h.join().unwrap();
        match err {
            TftpError::Remote { code, message } => {
                assert_eq!(code, 1);
                assert_eq!(message, "File not found");
            }
            other => panic!("unexpected error {other:?}"),
        }
    }

    #[test]
    fn timeout_when_nobody_answers() {
        // Bind a socket that never answers.
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = silent.local_addr().unwrap().port();
        let c = TftpConfig { timeout: Duration::from_millis(50), retries: 2, ..cfg(port) };
        let err = get(&c, "x", &mut Vec::new(), &mut |_, _| {}).unwrap_err();
        assert!(matches!(err, TftpError::Timeout { retries: 2 }), "{err:?}");
    }

    /// More than 65535 blocks: exercises block-number wraparound. ~32 MiB over loopback.
    #[test]
    #[ignore = "slow: transfers ~33 MiB in lock-step; run with --ignored"]
    fn loopback_block_wraparound_get_and_put() {
        let len = 65_537 * BLOCK_SIZE + 123;
        let data = pattern(len);
        let (port, h) = spawn_server(data.clone(), Default::default(), Default::default());
        let mut out = Vec::with_capacity(len);
        get(&cfg(port), "big", &mut out, &mut |_, _| {}).unwrap();
        h.join().unwrap();
        assert!(out == data, "GET content mismatch after wraparound");

        let stored = Arc::new(Mutex::new(Vec::with_capacity(len)));
        let (port, h) = spawn_server(Vec::new(), stored.clone(), Default::default());
        put(&cfg(port), "big", &mut &data[..], Some(len as u64), &mut |_, _| {}).unwrap();
        h.join().unwrap();
        assert!(*stored.lock().unwrap() == data, "PUT content mismatch after wraparound");
    }
}
