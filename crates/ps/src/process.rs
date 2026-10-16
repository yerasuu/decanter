//! Child processes (games, builds, conversions) with their output streamed
//! to whoever shows it.
//!
//! Tokio tasks rather than a thread per pipe: a game, its build and a
//! conversion can all run at once, and the runtime is already there.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use tokio::io::{AsyncBufReadExt as _, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Lines kept per job; older ones are dropped so a chatty game can't grow
/// its log without bound.
const MAX_LINES: usize = 5000;

pub enum Event {
    Line(String),
    /// `None` when waiting on the process failed.
    Exited(Option<ExitStatus>),
}

/// What a job does. Its name is what logs and log file names say, the same whatever the UI's
/// language; the UI words it itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Clone,
    Reset,
    Rollback,
    Update,
    Build,
    Game,
    Convert,
    Fsr411,
}

impl JobKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Clone => "clone",
            Self::Reset => "reset",
            Self::Rollback => "rollback",
            Self::Update => "update",
            Self::Build => "build",
            Self::Game => "game",
            Self::Convert => "convert",
            Self::Fsr411 => "fsr411",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running(u32),
    Exited(Option<i32>),
}

pub struct Job {
    pub kind: JobKind,
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
    pub fn failed(kind: JobKind, reason: String) -> Self {
        Self {
            kind,
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
            Event::Exited(status) => self.state = State::Exited(status.as_ref().and_then(ExitStatus::code)),
        }
    }

    /// SIGTERM to the whole process group: `run.sh` and the game it starts,
    /// nix-shell and what runs inside it, or a build's tools.
    pub fn stop(&mut self) {
        let State::Running(pid) = self.state else {
            return;
        };
        self.stop_requested = true;
        terminate_group(pid, self.kind);
    }
}

#[cfg(unix)]
fn terminate_group(pid: u32, kind: JobKind) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    if let Err(err) = killpg(Pid::from_raw(pid), Signal::SIGTERM) {
        tracing::warn!(pid, job = kind.name(), %err, "could not stop job");
    }
}

/// Jobs only run on Linux today (the ports and Proton); a job reaching
/// this elsewhere is a bug worth seeing, not a silent no-op.
#[cfg(not(unix))]
fn terminate_group(pid: u32, kind: JobKind) {
    tracing::error!(
        pid,
        job = kind.name(),
        "cannot stop a job's process group on this system"
    );
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

/// `command` inside a transient scope of its own, the way a PC game runs.
///
/// See [`decanter_core::own_scope`]; without a systemd user manager it is
/// `command` as it was. For a game only: a build or a conversion is
/// decanter's own work.
pub fn in_own_scope(command: Command, key: &str, title: &str) -> Command {
    match decanter_core::own_scope(key, title) {
        Some(scoped) => wrapped(scoped, &command),
        None => command,
    }
}

/// `command` under GameMode, through `gamemoded` (see
/// [`decanter_core::gamemode_prefix`]).
pub fn through_gamemode(command: Command, gamemoded: &Path) -> Command {
    let [shell, rest @ ..] = decanter_core::gamemode_prefix(gamemoded);
    let mut wrapper = std::process::Command::new(shell);
    wrapper.args(rest);
    wrapped(wrapper, &command)
}

/// `wrapper` followed by `original`'s program and arguments, with the
/// environment and working directory `original` was given.
fn wrapped(mut wrapper: std::process::Command, original: &Command) -> Command {
    let original = original.as_std();
    wrapper.arg(original.get_program()).args(original.get_args());
    for (name, value) in original.get_envs() {
        match value {
            Some(set) => wrapper.env(name, set),
            None => wrapper.env_remove(name),
        };
    }
    if let Some(dir) = original.get_current_dir() {
        wrapper.current_dir(dir);
    }
    Command::from(wrapper)
}

/// Starts `command` in its own process group with stdout and stderr
/// merged into events; the last event is always [`Event::Exited`].
/// Must be called inside the tokio runtime.
pub fn spawn(kind: JobKind, mut command: Command) -> io::Result<(Job, UnboundedReceiver<Event>)> {
    #[cfg(unix)]
    command.process_group(0);
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return Err(err),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let pid = child.id();
    Ok(follow(
        kind,
        pid,
        stdout,
        stderr,
        async move { child.wait().await.ok() },
    ))
}

/// A process started elsewhere, followed the way [`spawn`]'s are.
///
/// Its pipes come as lines, then [`Event::Exited`] with what `exit`
/// resolves to, once the pipes are drained. Must be called inside the
/// tokio runtime.
pub fn follow<O, E, X>(
    kind: JobKind,
    pid: Option<u32>,
    stdout: Option<O>,
    stderr: Option<E>,
    exit: X,
) -> (Job, UnboundedReceiver<Event>)
where
    O: AsyncRead + Unpin + Send + 'static,
    E: AsyncRead + Unpin + Send + 'static,
    X: Future<Output = Option<ExitStatus>> + Send + 'static,
{
    let (sender, receiver) = unbounded_channel();
    let readers = [
        stdout.map(|out| tokio::spawn(forward(out, sender.clone()))),
        stderr.map(|err| tokio::spawn(forward(err, sender.clone()))),
    ];
    tokio::spawn(async move {
        let status = exit.await;
        // Output written just before exit is still in the pipes; drain it first.
        for reader in readers.into_iter().flatten() {
            let _ = reader.await;
        }
        let _ = sender.send(Event::Exited(status));
    });
    let job = Job {
        kind,
        state: State::Running(pid.unwrap_or_default()),
        lines: VecDeque::new(),
        stop_requested: false,
        log_file: None,
    };
    (job, receiver)
}
