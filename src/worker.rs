//! Background workers: every network operation runs on its own thread and talks
//! to the UI exclusively through channels, so the UI thread never blocks.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::common::{RemoteEntry, human_bytes};
use crate::ftp::{FtpClient, FtpConfig};
use crate::sftp::{CommandOutput, SshClient, SshConfig};
use crate::tftp::{self, TftpConfig};

/// Callback used by workers to wake the UI (e.g. `egui::Context::request_repaint`).
pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Severity of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Error,
}

/// Messages from a worker to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    Log(Level, String),
    /// A long running operation started (`Some(label)`) or finished (`None`).
    Busy(Option<String>),
    Progress {
        done: u64,
        total: Option<u64>,
    },
    Connected(bool),
    Listing {
        cwd: String,
        entries: Vec<RemoteEntry>,
    },
    CommandOutput {
        output: CommandOutput,
        track_cwd: bool,
    },
}

/// Commands understood by the FTP and SSH session workers.
#[derive(Debug, Clone)]
pub enum SessionCommand {
    Refresh,
    /// Enter a sub directory (name relative to the cwd) or absolute path.
    Cd(String),
    Up,
    Upload {
        local: PathBuf,
        remote_name: String,
    },
    Download {
        remote_name: String,
        local: PathBuf,
    },
    Mkdir(String),
    Delete {
        name: String,
        is_dir: bool,
    },
    /// Run a remote command (SSH only). If `track_cwd` is set, the command's stdout
    /// is the new terminal working directory (used to emulate `cd`).
    Exec {
        command: String,
        track_cwd: bool,
    },
    Disconnect,
}

/// Handle to a worker thread owned by the UI.
pub struct WorkerHandle {
    tx: Option<Sender<SessionCommand>>,
    pub events: Receiver<Event>,
}

impl WorkerHandle {
    #[cfg(test)]
    pub(crate) fn test_channel() -> (Self, Receiver<SessionCommand>, Sender<Event>) {
        let (commands, receiver) = mpsc::channel();
        let (events, event_receiver) = mpsc::channel();
        (Self { tx: Some(commands), events: event_receiver }, receiver, events)
    }

    /// Queue a command; returns false if the worker has gone away.
    pub fn send(&self, cmd: SessionCommand) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.send(cmd).is_ok())
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        // Closing the channel makes the worker thread exit after its current job.
        self.tx.take();
    }
}

/// Sends events to the UI and wakes it up; throttles progress updates.
struct Reporter {
    tx: Sender<Event>,
    waker: Waker,
    last_progress: Instant,
}

impl Reporter {
    fn new(tx: Sender<Event>, waker: Waker) -> Self {
        Self { tx, waker, last_progress: Instant::now() - Duration::from_secs(1) }
    }
    fn send(&self, ev: Event) {
        let _ = self.tx.send(ev);
        (self.waker)();
    }
    fn info(&self, msg: impl Into<String>) {
        self.send(Event::Log(Level::Info, msg.into()));
    }
    fn ok(&self, msg: impl Into<String>) {
        self.send(Event::Log(Level::Success, msg.into()));
    }
    fn error(&self, msg: impl Into<String>) {
        self.send(Event::Log(Level::Error, msg.into()));
    }
    fn busy(&self, label: Option<&str>) {
        self.send(Event::Busy(label.map(str::to_string)));
    }
    fn progress(&mut self, done: u64, total: Option<u64>) {
        let finished = total.is_some_and(|t| done >= t);
        if finished || self.last_progress.elapsed() >= Duration::from_millis(100) {
            self.last_progress = Instant::now();
            self.send(Event::Progress { done, total });
        }
    }
}

/// Format an `anyhow` error including its whole cause chain on one line.
fn chain(e: &anyhow::Error) -> String {
    format!("{e:#}")
}

fn transfer_summary(bytes: u64, started: Instant) -> String {
    let secs = started.elapsed().as_secs_f64().max(0.001);
    format!("{} in {:.1}s ({}/s)", human_bytes(bytes), secs, human_bytes((bytes as f64 / secs) as u64))
}

// ------------------------------------------------------------------ FTP / SSH

/// Operations common to FTP and SSH sessions, so one worker loop serves both.
trait Session: Sized {
    const NAME: &'static str;
    fn cwd(&mut self) -> anyhow::Result<String>;
    fn cd(&mut self, path: &str) -> anyhow::Result<()>;
    fn up(&mut self) -> anyhow::Result<()>;
    fn list(&mut self) -> anyhow::Result<Vec<RemoteEntry>>;
    fn upload(
        &mut self,
        local: &std::path::Path,
        remote: &str,
        p: &mut crate::common::ProgressFn<'_>,
    ) -> anyhow::Result<u64>;
    fn download(
        &mut self,
        remote: &str,
        local: &std::path::Path,
        p: &mut crate::common::ProgressFn<'_>,
    ) -> anyhow::Result<u64>;
    fn mkdir(&mut self, name: &str) -> anyhow::Result<()>;
    fn delete(&mut self, name: &str, is_dir: bool) -> anyhow::Result<()>;
    fn exec(&mut self, _cmd: &str) -> anyhow::Result<CommandOutput> {
        anyhow::bail!("{} does not support remote commands", Self::NAME)
    }
    fn close(self) -> anyhow::Result<()>;
}

impl Session for FtpClient {
    const NAME: &'static str = "FTP";
    fn cwd(&mut self) -> anyhow::Result<String> {
        self.pwd()
    }
    fn cd(&mut self, path: &str) -> anyhow::Result<()> {
        FtpClient::cwd(self, path)
    }
    fn up(&mut self) -> anyhow::Result<()> {
        self.cdup()
    }
    fn list(&mut self) -> anyhow::Result<Vec<RemoteEntry>> {
        FtpClient::list(self)
    }
    fn upload(&mut self, l: &std::path::Path, r: &str, p: &mut crate::common::ProgressFn<'_>) -> anyhow::Result<u64> {
        FtpClient::upload(self, l, r, p)
    }
    fn download(&mut self, r: &str, l: &std::path::Path, p: &mut crate::common::ProgressFn<'_>) -> anyhow::Result<u64> {
        FtpClient::download(self, r, l, p)
    }
    fn mkdir(&mut self, name: &str) -> anyhow::Result<()> {
        FtpClient::mkdir(self, name)
    }
    fn delete(&mut self, name: &str, is_dir: bool) -> anyhow::Result<()> {
        FtpClient::delete(self, name, is_dir)
    }
    fn close(self) -> anyhow::Result<()> {
        self.quit()
    }
}

impl Session for SshClient {
    const NAME: &'static str = "SSH";
    fn cwd(&mut self) -> anyhow::Result<String> {
        Ok(SshClient::cwd(self).to_string())
    }
    fn cd(&mut self, path: &str) -> anyhow::Result<()> {
        SshClient::cd(self, path)
    }
    fn up(&mut self) -> anyhow::Result<()> {
        SshClient::up(self)
    }
    fn list(&mut self) -> anyhow::Result<Vec<RemoteEntry>> {
        SshClient::list(self)
    }
    fn upload(&mut self, l: &std::path::Path, r: &str, p: &mut crate::common::ProgressFn<'_>) -> anyhow::Result<u64> {
        SshClient::upload(self, l, r, p)
    }
    fn download(&mut self, r: &str, l: &std::path::Path, p: &mut crate::common::ProgressFn<'_>) -> anyhow::Result<u64> {
        SshClient::download(self, r, l, p)
    }
    fn exec(&mut self, cmd: &str) -> anyhow::Result<CommandOutput> {
        SshClient::exec(self, cmd)
    }
    fn mkdir(&mut self, name: &str) -> anyhow::Result<()> {
        SshClient::mkdir(self, name)
    }
    fn delete(&mut self, name: &str, is_dir: bool) -> anyhow::Result<()> {
        SshClient::delete(self, name, is_dir)
    }
    fn close(self) -> anyhow::Result<()> {
        self.disconnect()
    }
}

/// Start an FTP session worker that connects with `cfg`.
pub fn spawn_ftp(cfg: FtpConfig, waker: Waker) -> WorkerHandle {
    spawn_session(waker, move |rep: &Reporter| {
        rep.info(format!(
            "Connecting to ftp://{}:{} as {} ({} mode)…",
            cfg.host,
            cfg.port,
            cfg.username,
            if cfg.passive { "passive" } else { "active" }
        ));
        let client = FtpClient::connect(&cfg)?;
        if let Some(w) = client.welcome() {
            rep.info(format!("Server: {w}"));
        }
        Ok(client)
    })
}

/// Start an SSH/SFTP session worker that connects with `cfg`.
pub fn spawn_ssh(cfg: SshConfig, waker: Waker) -> WorkerHandle {
    spawn_session(waker, move |rep: &Reporter| {
        rep.info(format!("Connecting to ssh://{}@{}:{}…", cfg.username, cfg.host, cfg.port));
        let client = SshClient::connect(&cfg)?;
        rep.info(format!("Host key fingerprint: {} (not verified against known_hosts)", client.fingerprint()));
        Ok(client)
    })
}

fn spawn_session<S, F>(waker: Waker, connect: F) -> WorkerHandle
where
    S: Session + 'static,
    F: FnOnce(&Reporter) -> anyhow::Result<S> + Send + 'static,
{
    let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCommand>();
    let (ev_tx, ev_rx) = mpsc::channel::<Event>();
    let spawned = thread::Builder::new().name(format!("{}-worker", S::NAME.to_lowercase())).spawn(move || {
        let mut rep = Reporter::new(ev_tx, waker);
        rep.busy(Some("Connecting"));
        let mut session = match connect(&rep) {
            Ok(s) => s,
            Err(e) => {
                rep.error(format!("{} connection failed: {}", S::NAME, chain(&e)));
                rep.busy(None);
                rep.send(Event::Connected(false));
                return;
            }
        };
        rep.ok(format!("{} connected", S::NAME));
        rep.send(Event::Connected(true));
        send_listing(&mut session, &rep);
        rep.busy(None);

        while let Ok(cmd) = cmd_rx.recv() {
            if matches!(cmd, SessionCommand::Disconnect) {
                break;
            }
            run_session_command(&mut session, cmd, &mut rep);
            rep.busy(None);
        }
        match session.close() {
            Ok(()) => rep.ok(format!("{} disconnected", S::NAME)),
            Err(e) => rep.info(format!("{} disconnected ({})", S::NAME, chain(&e))),
        }
        rep.send(Event::Connected(false));
    });
    let tx = match spawned {
        Ok(_) => Some(cmd_tx),
        Err(e) => {
            // Extremely unlikely; report through a fresh channel.
            let (tx, rx) = mpsc::channel();
            let _ = tx.send(Event::Log(Level::Error, format!("cannot spawn worker thread: {e}")));
            let _ = tx.send(Event::Connected(false));
            return WorkerHandle { tx: None, events: rx };
        }
    };
    WorkerHandle { tx, events: ev_rx }
}

fn send_listing<S: Session>(s: &mut S, rep: &Reporter) {
    let cwd = s.cwd().unwrap_or_else(|_| "?".into());
    match s.list() {
        Ok(entries) => {
            rep.info(format!("Listed {cwd} ({} entries)", entries.len()));
            rep.send(Event::Listing { cwd, entries });
        }
        Err(e) => rep.error(format!("Listing {cwd} failed: {}", chain(&e))),
    }
}

fn run_session_command<S: Session>(s: &mut S, cmd: SessionCommand, rep: &mut Reporter) {
    match cmd {
        SessionCommand::Refresh => {
            rep.busy(Some("Listing"));
            send_listing(s, rep);
        }
        SessionCommand::Cd(path) => {
            rep.busy(Some("Changing directory"));
            match s.cd(&path) {
                Ok(()) => send_listing(s, rep),
                Err(e) => rep.error(chain(&e)),
            }
        }
        SessionCommand::Up => {
            rep.busy(Some("Changing directory"));
            match s.up() {
                Ok(()) => send_listing(s, rep),
                Err(e) => rep.error(chain(&e)),
            }
        }
        SessionCommand::Upload { local, remote_name } => {
            rep.busy(Some("Uploading"));
            rep.info(format!("Uploading {} → {remote_name}", local.display()));
            let started = Instant::now();
            let res = s.upload(&local, &remote_name, &mut |d, t| rep.progress(d, t));
            match res {
                Ok(n) => {
                    rep.ok(format!("Upload complete: {}", transfer_summary(n, started)));
                    send_listing(s, rep);
                }
                Err(e) => rep.error(format!("Upload failed: {}", chain(&e))),
            }
        }
        SessionCommand::Download { remote_name, local } => {
            rep.busy(Some("Downloading"));
            rep.info(format!("Downloading {remote_name} → {}", local.display()));
            let started = Instant::now();
            match s.download(&remote_name, &local, &mut |d, t| rep.progress(d, t)) {
                Ok(n) => rep.ok(format!("Download complete: {}", transfer_summary(n, started))),
                Err(e) => rep.error(format!("Download failed: {}", chain(&e))),
            }
        }
        SessionCommand::Mkdir(name) => {
            rep.busy(Some("Creating folder"));
            match s.mkdir(&name) {
                Ok(()) => {
                    rep.ok(format!("Created folder {name}"));
                    send_listing(s, rep);
                }
                Err(e) => rep.error(chain(&e)),
            }
        }
        SessionCommand::Delete { name, is_dir } => {
            rep.busy(Some("Deleting"));
            match s.delete(&name, is_dir) {
                Ok(()) => {
                    rep.ok(format!("Deleted {name}"));
                    send_listing(s, rep);
                }
                Err(e) => rep.error(chain(&e)),
            }
        }
        SessionCommand::Exec { command, track_cwd } => {
            rep.busy(Some("Running command"));
            match s.exec(&command) {
                Ok(output) => rep.send(Event::CommandOutput { output, track_cwd }),
                Err(e) => rep.error(format!("Command failed: {}", chain(&e))),
            }
        }
        SessionCommand::Disconnect => {}
    }
}

// ------------------------------------------------------------------ TFTP

/// Direction of a TFTP transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TftpOp {
    Get,
    Put,
}

/// Run a single TFTP transfer on a background thread. Events arrive on the returned receiver.
pub fn spawn_tftp(cfg: TftpConfig, op: TftpOp, remote: String, local: PathBuf, waker: Waker) -> Receiver<Event> {
    let (ev_tx, ev_rx) = mpsc::channel::<Event>();
    let tx2 = ev_tx.clone();
    let spawned = thread::Builder::new().name("tftp-worker".into()).spawn(move || {
        let mut rep = Reporter::new(ev_tx, waker);
        let started = Instant::now();
        let res = match op {
            TftpOp::Get => {
                rep.busy(Some("TFTP get"));
                rep.info(format!("TFTP GET {}:{} {remote} → {}", cfg.host, cfg.port, local.display()));
                tftp::get_file(&cfg, &remote, &local, &mut |d, t| rep.progress(d, t))
            }
            TftpOp::Put => {
                rep.busy(Some("TFTP put"));
                rep.info(format!("TFTP PUT {} → {}:{} {remote}", local.display(), cfg.host, cfg.port));
                tftp::put_file(&cfg, &local, &remote, &mut |d, t| rep.progress(d, t))
            }
        };
        match res {
            Ok(n) => {
                rep.progress(n, Some(n));
                rep.ok(format!("TFTP transfer complete: {}", transfer_summary(n, started)));
            }
            Err(e) => rep.error(format!("TFTP transfer failed: {e}")),
        }
        rep.busy(None);
    });
    if let Err(e) = spawned {
        let _ = tx2.send(Event::Log(Level::Error, format!("cannot spawn worker thread: {e}")));
        let _ = tx2.send(Event::Busy(None));
    }
    ev_rx
}
