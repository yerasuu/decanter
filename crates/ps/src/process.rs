//! Child processes (games, builds, conversions) with their output streamed
//! to whoever shows it.
//!
//! Tokio tasks rather than a thread per pipe: a game, its build and a
//! conversion can all run at once, and the runtime is already there.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Lines kept per job; older ones are dropped so a chatty game can't grow
/// its log without bound.
const MAX_LINES: usize = 5000;

pub enum Event {
    Line(String),
    /// The exit code, `None` when killed by a signal.
    Exited(Option<i32>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running(u32),
    Exited(Option<i32>),
}

pub struct Job {
    pub label: String,
    pub state: State,
    pub lines: VecDeque<String>,
    /// Set by [`Job::stop`], so the exit it causes is not taken for a failure.
    pub stop_requested: bool,
    /// Where the whole output goes ([`crate::run_log`]); `lines` keeps only the end.
    pub log_file: Option<PathBuf>,
}

impl Job {
    /// A job whose process never started, holding why, so its log reads
    /// like any other.
    pub fn failed(label: impl Into<String>, reason: String) -> Self {
        Self {
            label: label.into(),
            state: State::Exited(None),
            lines: VecDeque::from([reason]),
            stop_requested: false,
            log_file: None,
        }
    }

    pub const fn is_running(&self) -> bool {
        matches!(self.state, State::Running(_))
    }

    pub fn push(&mut self, event: Event) {
        match event {
            Event::Line(line) => {
                if self.lines.len() == MAX_LINES {
                    self.lines.pop_front();
                }
                self.lines.push_back(line);
            }
            Event::Exited(code) => self.state = State::Exited(code),
        }
    }

    /// SIGTERM to the whole process group: `run.sh` and the game it starts,
    /// nix-shell and what runs inside it, or a build's tools.
    pub fn stop(&mut self) {
        let State::Running(pid) = self.state else {
            return;
        };
        self.stop_requested = true;
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        if let Err(err) = killpg(Pid::from_raw(pid), Signal::SIGTERM) {
            tracing::warn!(pid, label = self.label, %err, "could not stop job");
        }
    }
}

/// Lines as they come, invalid UTF-8 replaced rather than ending the
/// stream: a game printing a stray byte must not go quiet in the log.
async fn forward(stream: impl AsyncRead + Unpin, sender: UnboundedSender<Event>) {
    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                let line = line.trim_end_matches(['\n', '\r']).to_owned();
                if sender.send(Event::Line(line)).is_err() {
                    break;
                }
            }
        }
    }
}

/// Starts `command` in its own process group with stdout and stderr
/// merged into events; the last event is always [`Event::Exited`].
/// Must be called inside the tokio runtime.
pub fn spawn(label: impl Into<String>, mut command: Command) -> io::Result<(Job, UnboundedReceiver<Event>)> {
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(false)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Err(err),
    };
    let (sender, receiver) = unbounded_channel();
    let stdout = child
        .stdout
        .take()
        .map(|out| tokio::spawn(forward(out, sender.clone())));
    let stderr = child
        .stderr
        .take()
        .map(|err| tokio::spawn(forward(err, sender.clone())));
    let pid = child.id().unwrap_or_default();
    tokio::spawn(async move {
        let code = child.wait().await.ok().and_then(|status| status.code());
        // Output written just before exit is still in the pipes; drain it first.
        for reader in [stdout, stderr].into_iter().flatten() {
            let _ = reader.await;
        }
        let _ = sender.send(Event::Exited(code));
    });
    let job = Job {
        label: label.into(),
        state: State::Running(pid),
        lines: VecDeque::new(),
        stop_requested: false,
        log_file: None,
    };
    Ok((job, receiver))
}
