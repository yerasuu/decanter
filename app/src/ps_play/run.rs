//! A port job's run outside GPUI: how it starts ([`Launcher`]), its log file, its end, what the
//! scanner made of it, and what a game's run leaves recorded (failures, what the port learned, the
//! frame generation picked).

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Instant;

use decanter_nix::Toolchain;
use decanter_ps::bbport::LauncherSettings;
use decanter_ps::bbport::frame_gen::{self, FrameGen};
use decanter_ps::failures::{self, CallReport, Failure, Log, Scanner};
use decanter_ps::learned;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::{self, Event, Job, JobKind};
use decanter_ps::run_log::{RunLog, describe_exit};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedReceiver;

pub struct Finished {
    /// `None` when it never started or was killed by a signal.
    pub code: Option<i32>,
    /// What the scanner found, when the job had one.
    pub failures: Vec<Failure>,
    /// The port's line turning frame overlap off during the run, when it printed one.
    pub frame_overlap_off: Option<String>,
    /// The last frame generation the game's menu switched to during the run.
    pub frame_gen: Option<FrameGen>,
}

impl Finished {
    pub const fn succeeded(&self) -> bool {
        matches!(self.code, Some(0))
    }

    /// A job that never started, or whose window went away before it was followed.
    pub const fn none() -> Self {
        Self {
            code: None,
            failures: Vec::new(),
            frame_overlap_off: None,
            frame_gen: None,
        }
    }
}

/// How a job's process starts.
pub enum Launcher {
    /// In a process group of its own, which [`Job::stop`] ends.
    Command(Command),
    /// A game in a native session of its own (a Job Object), which only the session's stop reaches.
    #[cfg(windows)]
    Session(decanter_runtime::Prepared),
}

impl Launcher {
    pub const fn command_mut(&mut self) -> &mut Command {
        match self {
            Self::Command(command) => command,
            #[cfg(windows)]
            Self::Session(prepared) => prepared.command_mut(),
        }
    }

    pub fn program(&mut self) -> String {
        self.command_mut().as_std().get_program().to_string_lossy().into_owned()
    }

    #[cfg(windows)]
    pub fn session_key(&self) -> Option<String> {
        match self {
            Self::Command(_) => None,
            Self::Session(prepared) => Some(prepared.key().to_owned()),
        }
    }

    /// Must be called inside the tokio runtime, as [`process::spawn`].
    pub fn spawn(self, kind: JobKind) -> io::Result<(Job, UnboundedReceiver<Event>)> {
        match self {
            Self::Command(command) => process::spawn(kind, command),
            #[cfg(windows)]
            Self::Session(prepared) => {
                let mut session = match prepared.spawn() {
                    Ok(session) => session,
                    Err(err) => return Err(io::Error::other(format!("{err:#}"))),
                };
                let (stdout, stderr) = session.take_output();
                let pid = session.launcher_pid();
                Ok(process::follow(kind, pid, stdout, stderr, async move {
                    session
                        .wait_status()
                        .await
                        .inspect_err(|err| {
                            tracing::warn!(job = kind.name(), error = %format_args!("{err:#}"), "could not wait for the session");
                        })
                        .ok()
                }))
            }
        }
    }
}

/// What a job's log header says about `port`: its commit before the job, when it is checked
/// out, and the toolchain the job runs in, when it runs in one.
pub async fn port_facts(root: &Root, port: Port, toolchain: Option<&Toolchain>) -> Vec<(&'static str, String)> {
    let mut facts = vec![("port", port.name().to_owned())];
    let commit = failures::full_commit_of(&port.checkout(root)).await;
    if !commit.is_empty() {
        facts.push(("port_commit", commit));
    }
    if let Some(toolchain) = toolchain {
        facts.push(("toolchain", toolchain.id().to_owned()));
    }
    facts
}

/// What a game's job logs are named after ([`RunLog::start`]'s `target`): its dump's folder.
pub fn game_target(dir: &Path) -> String {
    dir.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// Drops the file once it stops taking lines, so the job runs on without it.
pub async fn log_lines(log: &mut Option<RunLog>, batch: &[Event], kind: JobKind, target: &str) {
    if let Some(file) = log.as_mut() {
        for event in batch {
            if let Event::Line(line) = event
                && let Err(err) = file.line(line).await
            {
                tracing::warn!(job = kind.name(), target, %err, "the job's log file stopped taking lines");
                *log = None;
                break;
            }
        }
    }
}

/// Ends the log, and copies it to the debug log folder when debug mode is on.
pub async fn finish_log(
    file: RunLog,
    exit: Option<ExitStatus>,
    report: Option<&CallReport>,
    calls: Option<PathBuf>,
    kind: JobKind,
    target: &str,
) {
    let path = file.path().to_owned();
    match file.finish(exit, report).await {
        Ok(()) => crate::debug_logs::keep_job(path, calls).await,
        Err(err) => tracing::warn!(job = kind.name(), target, %err, "could not finish the job's log file"),
    }
}

pub fn finished(
    scanner: Option<Scanner>,
    exit: Option<ExitStatus>,
    stopped: bool,
    log: Option<&Path>,
    report: Option<CallReport>,
) -> Finished {
    let frame_overlap_off = scanner
        .as_ref()
        .and_then(|scanner| scanner.frame_overlap_off().map(str::to_owned));
    let frame_gen = scanner.as_ref().and_then(Scanner::frame_gen);
    Finished {
        code: exit.and_then(|status| status.code()),
        failures: scanner.map_or_else(Vec::new, |scanner| scanner.exit(exit, stopped, log, report)),
        frame_overlap_off,
        frame_gen,
    }
}

pub fn trace_end(
    kind: JobKind,
    target: &str,
    exit: Option<ExitStatus>,
    stopped: bool,
    is_killed: bool,
    started: Instant,
    log: Option<&Path>,
) {
    let duration_ms = started.elapsed().as_millis();
    let succeeded = exit.is_some_and(|status| status.success());
    let exit = if is_killed {
        format!(
            "killed by decanter, exit code {:#010X}",
            decanter_runtime::KILLED_EXIT_CODE
        )
    } else {
        describe_exit(exit)
    };
    let log = log.map(|path| tracing::field::display(path.display()));
    if is_killed {
        tracing::warn!(
            job = kind.name(),
            target,
            exit,
            duration_ms,
            log,
            "job ended by decanter killing it after a stop request"
        );
    } else if succeeded || stopped {
        tracing::info!(
            job = kind.name(),
            target,
            exit,
            stopped,
            duration_ms,
            log,
            "job finished"
        );
    } else {
        tracing::warn!(job = kind.name(), target, exit, duration_ms, log, "job failed");
    }
}

/// `None` without a report: a port that predates it, or one that ended before writing it.
pub async fn read_call_report(path: Option<&Path>) -> Option<CallReport> {
    let Some(path) = path else {
        return None;
    };
    match tokio::fs::read_to_string(path).await {
        Ok(text) => CallReport::parse(&text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "could not read the port's call report");
            None
        }
    }
}

/// Where a profile's failures are kept, which the Fallos list and `decanter ps report` read.
pub fn failures_path(root: &Root, profile_id: &str) -> PathBuf {
    root.profile(profile_id).join("failures.jsonl")
}

/// Appends a run's failures to the profile's record at `path`.
pub async fn save_failures(path: PathBuf, found: Vec<Failure>) -> io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut log = Log::load(path);
        log.append(found)
    })
    .await
    .unwrap_or_else(|err| Err(io::Error::other(err)))
}

/// The port caught the game rewriting frames in flight and stopped overlapping them; the next
/// run starts that way, so its first frames are not corrupted while the guard catches it again.
pub async fn save_frame_overlap_off(root: &Root, title_id: &str, line: String) -> io::Result<()> {
    tracing::info!(
        title_id,
        line,
        "the port turned frame overlap off: next runs start without it"
    );
    let (root, title_id) = (root.clone(), title_id.to_owned());
    tokio::task::spawn_blocking(move || learned::Store::record_frame_overlap_off(&root, &title_id, line))
        .await
        .unwrap_or_else(|err| Err(io::Error::other(err)))
}

/// The last choice made in the game's menu becomes the title's setting, so the Graphics tab shows it
/// and the next run starts with it. Off removes the key, like the tab's own row.
pub async fn save_frame_gen(root: &Root, title_id: &str, frame_gen: FrameGen) {
    let saved = {
        let (root, title_id) = (root.clone(), title_id.to_owned());
        tokio::task::spawn_blocking(move || {
            LauncherSettings::edit(&root, |settings| {
                let value = if frame_gen == FrameGen::Off {
                    ""
                } else {
                    frame_gen.value()
                };
                settings.set_title_text(&title_id, frame_gen::KEY, value);
            })
        })
        .await
        .unwrap_or_else(|err| Err(io::Error::other(err)))
    };
    match saved {
        Ok(()) => tracing::info!(
            title_id,
            value = frame_gen.value(),
            "saved the frame generation chosen in the menu"
        ),
        Err(err) => tracing::warn!(title_id, %err, "could not save the frame generation chosen in the menu"),
    }
}
