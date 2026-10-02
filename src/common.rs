//! Types and helpers shared by all protocol back-ends.

use std::io::{self, Read, Write};

/// A single entry of a remote directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    /// Size in bytes, if known.
    pub size: Option<u64>,
    /// Last modification time as Unix seconds (UTC), if known.
    pub modified: Option<u64>,
}

impl RemoteEntry {
    /// Sort helper: directories first, then case-insensitive by name.
    pub fn sort_listing(entries: &mut [RemoteEntry]) {
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    }
}

/// Progress callback: `(bytes_done, total_bytes_if_known)`.
pub type ProgressFn<'a> = dyn FnMut(u64, Option<u64>) + 'a;

/// Copy all bytes from `reader` to `writer`, calling `progress` after every chunk.
pub fn copy_with_progress<R: Read + ?Sized, W: Write + ?Sized>(
    reader: &mut R,
    writer: &mut W,
    total: Option<u64>,
    progress: &mut ProgressFn<'_>,
) -> io::Result<u64> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    progress(0, total);
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        writer.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    writer.flush()?;
    Ok(done)
}

/// Join a remote (always `/`-separated) directory and a child name.
pub fn join_remote(dir: &str, name: &str) -> String {
    if name.starts_with('/') {
        name.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Human readable byte count (e.g. `1.5 MiB`).
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}

/// Format Unix seconds as `YYYY-MM-DD HH:MM` (UTC) without pulling in a date crate.
pub fn format_unix_time(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem / 60 % 60)
}

/// Quote a string for a POSIX shell (single quotes).
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Resolve `host:port` into a socket address (first result).
pub fn resolve(host: &str, port: u16) -> anyhow::Result<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    let host = host.trim();
    if host.is_empty() {
        anyhow::bail!("host is empty");
    }
    // Allow bare IPv6 literals without brackets.
    let target = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    target
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("cannot resolve {target}: {e}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("no address found for {target}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_works() {
        assert_eq!(join_remote("/", "a"), "/a");
        assert_eq!(join_remote("/home", "a"), "/home/a");
        assert_eq!(join_remote("/home/", "a"), "/home/a");
        assert_eq!(join_remote("/home", "/etc"), "/etc");
    }

    #[test]
    fn human_bytes_works() {
        assert_eq!(human_bytes(10), "10 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
    }

    #[test]
    fn copy_reports_progress() {
        let data = vec![7u8; 200_000];
        let mut out = Vec::new();
        let mut last = 0;
        let n = copy_with_progress(&mut &data[..], &mut out, Some(200_000), &mut |d, _| last = d).unwrap();
        assert_eq!(n, 200_000);
        assert_eq!(last, 200_000);
        assert_eq!(out, data);
    }

    #[test]
    fn sort_dirs_first() {
        let mut v = vec![
            RemoteEntry { name: "b".into(), is_dir: false, size: None, modified: None },
            RemoteEntry { name: "Z".into(), is_dir: true, size: None, modified: None },
            RemoteEntry { name: "a".into(), is_dir: false, size: None, modified: None },
        ];
        RemoteEntry::sort_listing(&mut v);
        let names: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Z", "a", "b"]);
    }

    #[test]
    fn unix_time_formatting() {
        assert_eq!(format_unix_time(0), "1970-01-01 00:00");
        assert_eq!(format_unix_time(951_782_400), "2000-02-29 00:00");
        assert_eq!(format_unix_time(1_790_000_000), "2026-09-21 14:13");
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quote("/home/a b"), "'/home/a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn resolve_localhost() {
        assert!(resolve("127.0.0.1", 21).is_ok());
        assert!(resolve("::1", 21).is_ok());
        assert!(resolve("", 21).is_err());
    }
}
