//! Per-tab session state (one tab per open connection) and event handling.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::common::{RemoteEntry, shell_quote};
use crate::config::{Protocol, SavedSession};
use crate::ftp::FtpConfig;
use crate::sftp::{SshAuth, SshConfig};
use crate::tftp::TftpConfig;
use crate::worker::{self, Event, Level, SessionCommand, TftpOp, Waker, WorkerHandle};

const MAX_LOG_LINES: usize = 2000;
const MAX_TERM_LINES: usize = 5000;

/// Secrets typed into the "new session" dialog. Kept in memory only.
#[derive(Clone, Default)]
pub struct Secrets {
    pub password: String,
    pub passphrase: String,
}

pub struct LogLine {
    pub at: Duration,
    pub level: Level,
    pub text: String,
}

/// Remote directory browser state (used by the sidebar and the SFTP/FTP tabs).
#[derive(Default)]
pub struct Browser {
    pub cwd: String,
    pub entries: Vec<RemoteEntry>,
    pub selected: Option<usize>,
    pub path_input: String,
    pub upload_local: String,
    pub upload_remote_name: String,
    pub download_local: String,
    pub new_folder: Option<String>,
}

impl Browser {
    pub fn selected_entry(&self) -> Option<&RemoteEntry> {
        self.selected.and_then(|i| self.entries.get(i))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TermKind {
    Prompt,
    Stdout,
    Stderr,
    Info,
}

pub struct TermLine {
    pub kind: TermKind,
    pub text: String,
}

/// A simple line-oriented terminal: each command runs in its own SSH exec channel.
#[derive(Default)]
pub struct Terminal {
    pub lines: Vec<TermLine>,
    pub input: String,
    /// Emulated working directory (each command is prefixed with `cd <cwd> &&`).
    pub cwd: String,
    pub history: Vec<String>,
    pub history_pos: Option<usize>,
    pub want_focus: bool,
}

impl Terminal {
    pub fn push(&mut self, kind: TermKind, text: &str) {
        for l in text.split('\n') {
            self.lines.push(TermLine { kind, text: l.trim_end_matches('\r').to_string() });
        }
        if self.lines.len() > MAX_TERM_LINES {
            let excess = self.lines.len() - MAX_TERM_LINES;
            self.lines.drain(..excess);
        }
    }
}

/// TFTP transfer form state.
pub struct TftpForm {
    pub remote: String,
    pub local: String,
    pub timeout_secs: u32,
    pub retries: u32,
    pub events: Option<Receiver<Event>>,
}

/// Connection state shown in tabs and the status bar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    Disconnected,
    /// TFTP is connection-less.
    Ready,
}

pub struct SessionTab {
    pub id: u64,
    pub info: SavedSession,
    pub secrets: Secrets,
    pub worker: Option<WorkerHandle>,
    pub connected: bool,
    pub busy: Option<String>,
    pub progress: Option<(u64, Option<u64>)>,
    pub browser: Browser,
    pub term: Terminal,
    pub tftp: TftpForm,
    pub log: Vec<LogLine>,
    started: Instant,
    waker: Waker,
}

impl SessionTab {
    /// Create the tab and (for SSH/SFTP/FTP) start connecting immediately.
    pub fn open(id: u64, info: SavedSession, secrets: Secrets, waker: Waker) -> Self {
        let mut tab = Self {
            id,
            info,
            secrets,
            worker: None,
            connected: false,
            busy: None,
            progress: None,
            browser: Browser::default(),
            term: Terminal { want_focus: true, ..Default::default() },
            tftp: TftpForm { remote: String::new(), local: String::new(), timeout_secs: 3, retries: 5, events: None },
            log: Vec::new(),
            started: Instant::now(),
            waker,
        };
        tab.connect();
        tab
    }

    pub fn title(&self) -> String {
        self.info.display_name()
    }

    pub fn supports_files(&self) -> bool {
        self.info.protocol != Protocol::Tftp
    }

    pub fn state(&self) -> ConnState {
        if self.info.protocol == Protocol::Tftp {
            ConnState::Ready
        } else if self.connected {
            ConnState::Connected
        } else if self.worker.is_some() {
            ConnState::Connecting
        } else {
            ConnState::Disconnected
        }
    }

    pub fn is_idle(&self) -> bool {
        self.connected && self.busy.is_none()
    }

    pub fn is_busy(&self) -> bool {
        self.busy.is_some() || self.tftp.events.is_some() || self.state() == ConnState::Connecting
    }

    pub fn log(&mut self, level: Level, text: impl Into<String>) {
        self.log.push(LogLine { at: self.started.elapsed(), level, text: text.into() });
        if self.log.len() > MAX_LOG_LINES {
            let excess = self.log.len() - MAX_LOG_LINES;
            self.log.drain(..excess);
        }
    }

    /// (Re)connect for connection-oriented protocols.
    pub fn connect(&mut self) {
        let i = &self.info;
        match i.protocol {
            Protocol::Tftp => {
                let msg = format!("TFTP target {}:{} (UDP, no login required)", i.host, i.port);
                self.log(Level::Info, msg);
            }
            Protocol::Ftp => {
                let cfg = FtpConfig {
                    host: i.host.clone(),
                    port: i.port,
                    username: i.user.clone(),
                    password: self.secrets.password.clone(),
                    passive: i.passive,
                    timeout: Duration::from_secs(15),
                };
                self.worker = Some(worker::spawn_ftp(cfg, self.waker.clone()));
                self.busy = Some("Connecting".into());
            }
            Protocol::Ssh | Protocol::Sftp => {
                let auth = match i.auth {
                    crate::config::AuthMethod::Password => SshAuth::Password(self.secrets.password.clone()),
                    crate::config::AuthMethod::KeyFile => SshAuth::KeyFile {
                        path: PathBuf::from(i.key_path.trim()),
                        passphrase: Some(self.secrets.passphrase.clone()).filter(|p| !p.is_empty()),
                    },
                };
                let cfg = SshConfig {
                    host: i.host.clone(),
                    port: i.port,
                    username: i.user.clone(),
                    auth,
                    timeout: Duration::from_secs(15),
                };
                self.worker = Some(worker::spawn_ssh(cfg, self.waker.clone()));
                self.busy = Some("Connecting".into());
                if i.protocol == Protocol::Ssh {
                    let msg = format!("Connecting to {}@{}:{} …", i.user, i.host, i.port);
                    self.term.push(TermKind::Info, &msg);
                }
            }
        }
    }

    pub fn send(&self, cmd: SessionCommand) {
        if let Some(w) = &self.worker {
            w.send(cmd);
        }
    }

    pub fn disconnect(&mut self) {
        if self.worker.is_some() {
            self.send(SessionCommand::Disconnect);
        }
    }

    /// Handle a line typed into the terminal.
    pub fn run_terminal_line(&mut self) {
        let line = std::mem::take(&mut self.term.input);
        let cmd = line.trim().to_string();
        self.term.history_pos = None;
        let prompt = self.prompt();
        self.term.push(TermKind::Prompt, &format!("{prompt}{line}"));
        if cmd.is_empty() {
            return;
        }
        if self.term.history.last() != Some(&cmd) {
            self.term.history.push(cmd.clone());
        }
        if cmd == "clear" || cmd == "cls" {
            self.term.lines.clear();
            return;
        }
        if !self.connected {
            self.term.push(TermKind::Stderr, "Not connected. Use Session → Reconnect.");
            return;
        }
        let prefix =
            if self.term.cwd.is_empty() { String::new() } else { format!("cd {} && ", shell_quote(&self.term.cwd)) };
        let is_cd = cmd == "cd" || cmd.starts_with("cd ");
        let command = if is_cd { format!("{prefix}{cmd} && pwd") } else { format!("{prefix}{cmd}") };
        self.send(SessionCommand::Exec { command, track_cwd: is_cd });
    }

    pub fn prompt(&self) -> String {
        let cwd = if self.term.cwd.is_empty() { "~" } else { &self.term.cwd };
        format!("{}@{}:{}$ ", self.info.user, self.info.host, cwd)
    }

    pub fn start_tftp(&mut self, op: TftpOp) {
        let mut local = self.tftp.local.trim().to_string();
        let remote = self.tftp.remote.trim().to_string();
        if remote.is_empty() {
            self.log(Level::Error, "Please enter the remote file name.");
            return;
        }
        if local.is_empty() && op == TftpOp::Get {
            let name = remote.rsplit(['/', '\\']).next().unwrap_or("download.bin").to_string();
            local = super::default_local_dir().join(name).to_string_lossy().into_owned();
            self.tftp.local = local.clone();
        }
        if local.is_empty() {
            self.log(Level::Error, "Please choose a local file to upload.");
            return;
        }
        let cfg = TftpConfig {
            host: self.info.host.clone(),
            port: self.info.port,
            timeout: Duration::from_secs(u64::from(self.tftp.timeout_secs.max(1))),
            retries: self.tftp.retries,
        };
        self.progress = None;
        self.busy = Some("Starting".into());
        self.tftp.events = Some(worker::spawn_tftp(cfg, op, remote, PathBuf::from(local), self.waker.clone()));
    }

    /// Drain events from background workers.
    pub fn poll(&mut self) {
        let mut events = Vec::new();
        let mut worker_gone = false;
        if let Some(w) = &self.worker {
            loop {
                match w.events.try_recv() {
                    Ok(ev) => events.push(ev),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        worker_gone = true;
                        break;
                    }
                }
            }
        }
        let mut tftp_done = false;
        if let Some(rx) = &self.tftp.events {
            loop {
                match rx.try_recv() {
                    Ok(ev) => events.push(ev),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        tftp_done = true;
                        break;
                    }
                }
            }
        }
        for ev in events {
            self.handle(ev);
        }
        if tftp_done {
            self.tftp.events = None;
            self.busy = None;
        }
        if worker_gone {
            self.worker = None;
            self.connected = false;
            self.busy = None;
            self.progress = None;
            self.browser.entries.clear();
            self.browser.selected = None;
        }
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Log(level, text) => {
                if self.info.protocol == Protocol::Ssh && level == Level::Error {
                    self.term.push(TermKind::Stderr, &text);
                }
                self.log(level, text);
            }
            Event::Busy(b) => {
                if b.is_none() {
                    self.progress = None;
                }
                self.busy = b;
            }
            Event::Progress { done, total } => self.progress = Some((done, total)),
            Event::Connected(c) => {
                self.connected = c;
                if self.info.protocol == Protocol::Ssh {
                    let msg =
                        if c { "Connected. Commands run via SSH exec channels (no PTY)." } else { "Session closed." };
                    self.term.push(TermKind::Info, msg);
                    if c {
                        // Learn the shell's start directory (usually $HOME).
                        self.term.cwd.clear();
                        self.send(SessionCommand::Exec { command: "pwd".into(), track_cwd: true });
                    }
                }
            }
            Event::Listing { cwd, entries } => {
                self.browser.path_input = cwd.clone();
                self.browser.cwd = cwd;
                self.browser.entries = entries;
                self.browser.selected = None;
            }
            Event::CommandOutput { output, track_cwd } => {
                if track_cwd && output.exit_code == 0 {
                    let new = output.stdout.trim();
                    if !new.is_empty() {
                        self.term.cwd = new.to_string();
                    }
                } else if !output.stdout.is_empty() {
                    self.term.push(TermKind::Stdout, output.stdout.trim_end_matches('\n'));
                }
                if !output.stderr.is_empty() {
                    self.term.push(TermKind::Stderr, output.stderr.trim_end_matches('\n'));
                }
                if output.exit_code != 0 {
                    self.term.push(TermKind::Info, &format!("[exit code {}]", output.exit_code));
                }
                self.log(Level::Info, format!("command finished with exit code {}", output.exit_code));
            }
        }
    }
}
