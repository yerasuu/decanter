//! Running a port's process to completion with its output in an entity of
//! its own, so a chatty game redraws only the log that shows it.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Instant;

use decanter_ps::failures::{self, CallReport, Failure, Scanner};
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::{self, Event, Job, JobKind};
use decanter_ps::run_log::{self, RunLog, describe_exit};
use decanter_ps::toolchain::Toolchain;
use gpui::{AppContext as _, AsyncApp, Entity, WeakEntity};
use rust_i18n::t;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedReceiver;

use super::Ps;

/// Lines taken per redraw: a burst of output costs one notify, not one per line.
const BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JobKey {
    /// Cloning, building or updating a port.
    Port(Port),
    /// Playing or converting the game in this folder.
    Game(PathBuf),
    /// Recording FSR 4.1.1 from an AMD DLL.
    Fsr411,
}

pub struct Finished {
    /// `None` when it never started or was killed by a signal.
    pub code: Option<i32>,
    /// What the scanner found, when the job had one.
    pub failures: Vec<Failure>,
    /// The port's line turning frame overlap off during the run, when it printed one.
    pub frame_overlap_off: Option<String>,
}

impl Finished {
    pub const fn succeeded(&self) -> bool {
        matches!(self.code, Some(0))
    }
}

/// What the UI calls a job of `kind`.
pub fn label(kind: JobKind) -> Cow<'static, str> {
    match kind {
        JobKind::Clone => t!("ps.jobs.clone"),
        JobKind::Reset => t!("ps.jobs.reset"),
        JobKind::Rollback => t!("ps.jobs.rollback"),
        JobKind::Update => t!("ps.jobs.update"),
        JobKind::Build => t!("ps.jobs.build"),
        JobKind::Game => t!("ps.jobs.game"),
        JobKind::Convert => t!("ps.jobs.convert"),
        JobKind::Fsr411 => t!("ps.jobs.fsr411"),
        JobKind::VkLayer => t!("ps.jobs.vk_layer"),
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

/// How a job's process starts.
pub enum Launcher {
    /// In a process group of its own, which [`Job::stop`] ends.
    Command(Command),
    /// A game in a native session of its own (a Job Object), which only the session's stop reaches.
    #[cfg(windows)]
    Session(decanter_runtime::Prepared),
}

impl Launcher {
    const fn command_mut(&mut self) -> &mut Command {
        match self {
            Self::Command(command) => command,
            #[cfg(windows)]
            Self::Session(prepared) => prepared.command_mut(),
        }
    }

    fn program(&mut self) -> String {
        self.command_mut().as_std().get_program().to_string_lossy().into_owned()
    }

    #[cfg(windows)]
    fn session_key(&self) -> Option<String> {
        match self {
            Self::Command(_) => None,
            Self::Session(prepared) => Some(prepared.key().to_owned()),
        }
    }

    /// Must be called inside the tokio runtime, as [`process::spawn`].
    fn spawn(self, kind: JobKind) -> io::Result<(Job, UnboundedReceiver<Event>)> {
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

/// [`run_launcher`] for a job started as a plain command.
#[expect(
    clippy::too_many_arguments,
    reason = "the library is the caller's, fixed when its job started"
)]
pub async fn run(
    ps: &WeakEntity<Ps>,
    root: &Root,
    key: JobKey,
    kind: JobKind,
    facts: Vec<(&'static str, String)>,
    command: Command,
    scanner: Option<Scanner>,
    cx: &mut AsyncApp,
) -> Finished {
    run_launcher(ps, root, key, kind, facts, Launcher::Command(command), scanner, cx).await
}

/// Replaces whatever job `key` held before; its log goes with it. `facts` go in the log's
/// header, before what `scanner` knows about the game.
#[expect(
    clippy::too_many_arguments,
    reason = "the library is the caller's, fixed when its job started"
)]
pub async fn run_launcher(
    ps: &WeakEntity<Ps>,
    root: &Root,
    key: JobKey,
    kind: JobKind,
    mut facts: Vec<(&'static str, String)>,
    mut launcher: Launcher,
    mut scanner: Option<Scanner>,
    cx: &mut AsyncApp,
) -> Finished {
    let target = target_name(&key);
    facts.extend(scanner.as_ref().map(Scanner::facts).into_iter().flatten());
    // A job runs without its file rather than not at all; the log view says so.
    let started_log = RunLog::start(
        &run_log::dir(root),
        &target,
        kind,
        &facts,
        launcher.command_mut(),
        scanner.is_some(),
    )
    .await;
    let (mut log, log_error) = match started_log {
        Ok(log) => (Some(log), None),
        Err(err) => {
            tracing::warn!(job = kind.name(), target, %err, "could not create the job's log file");
            (None, Some(t!("ps.jobs.log_error", error = err).into_owned()))
        }
    };
    let program = launcher.program();
    #[cfg(windows)]
    let session_key = launcher.session_key();
    let (mut job, mut events) = match launcher.spawn(kind) {
        Ok(spawned) => spawned,
        Err(err) => {
            tracing::warn!(job = kind.name(), target, program, %err, "could not start job");
            if let Some(mut file) = log {
                let path = file.path().to_owned();
                let written = file.line(&format!("# error: could not start {program}: {err}")).await;
                match written.and(file.finish(None, None).await) {
                    Ok(()) => crate::debug_logs::keep_job(path, None).await,
                    Err(err) => tracing::warn!(job = kind.name(), target, %err, "could not finish the job's log file"),
                }
            }
            let job = cx.new(|_| Job::failed(kind, t!("ps.jobs.start_error", error = err).into_owned()));
            let _ = ps.update(cx, |ps, cx| {
                ps.jobs.insert(key, job);
                cx.notify();
            });
            return Finished {
                code: None,
                failures: Vec::new(),
                frame_overlap_off: None,
            };
        }
    };
    let log_path = log.as_ref().map(|log| log.path().to_owned());
    let calls_path = log.as_ref().and_then(|log| log.calls_path().map(Path::to_owned));
    tracing::info!(
        job = kind.name(),
        target,
        program,
        log = log_path.as_deref().map(|path| tracing::field::display(path.display())),
        "job started"
    );
    let started = Instant::now();
    job.log_file.clone_from(&log_path);
    if let Some(error) = log_error {
        job.push(Event::Line(error));
    }
    let job: Entity<Job> = cx.new(|_| job);
    if ps
        .update(cx, |ps, cx| {
            ps.jobs.insert(key, job.clone());
            cx.notify();
        })
        .is_err()
    {
        return Finished {
            code: None,
            failures: Vec::new(),
            frame_overlap_off: None,
        };
    }
    let mut batch = Vec::with_capacity(BATCH);
    let mut exit = None;
    while events.recv_many(&mut batch, BATCH).await > 0 {
        log_lines(&mut log, &batch, kind, &target).await;
        job.update(cx, |job, cx| {
            for event in batch.drain(..) {
                match (&event, scanner.as_mut()) {
                    (Event::Line(line), Some(scanner)) => scanner.line(line),
                    (Event::Exited(status), _) => exit = *status,
                    (Event::Line(_), None) => {}
                }
                job.push(event);
            }
            cx.notify();
        });
    }
    let report = read_call_report(calls_path.as_deref()).await;
    #[cfg(windows)]
    let killed_at = session_key.as_deref().and_then(decanter_runtime::take_killed);
    #[cfg(not(windows))]
    let killed_at: Option<std::time::SystemTime> = None;
    if let Some(scanner) = scanner.as_mut() {
        scanner.set_killed(killed_at, decanter_runtime::KILLED_EXIT_CODE);
    }
    if let Some(mut file) = log {
        file.set_killed(killed_at, decanter_runtime::KILLED_EXIT_CODE);
        finish_log(file, exit, report.as_ref(), calls_path, kind, &target).await;
    }
    let stopped = job.read_with(cx, |job, _| job.stop_requested);
    trace_end(
        kind,
        &target,
        exit,
        stopped,
        killed_at.is_some(),
        started,
        log_path.as_deref(),
    );
    let _ = ps.update(cx, |_, cx| cx.notify());
    finished(scanner, exit, stopped, log_path.as_deref(), report)
}

/// Ends the log, and copies it to the debug log folder when debug mode is on.
async fn finish_log(
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

fn finished(
    scanner: Option<Scanner>,
    exit: Option<ExitStatus>,
    stopped: bool,
    log: Option<&Path>,
    report: Option<CallReport>,
) -> Finished {
    let frame_overlap_off = scanner
        .as_ref()
        .and_then(|scanner| scanner.frame_overlap_off().map(str::to_owned));
    Finished {
        code: exit.and_then(|status| status.code()),
        failures: scanner.map_or_else(Vec::new, |scanner| scanner.exit(exit, stopped, log, report)),
        frame_overlap_off,
    }
}

/// What a job's log files are named after ([`RunLog::start`]'s `target`).
pub fn target_name(key: &JobKey) -> String {
    match key {
        JobKey::Port(port) => port.name().to_lowercase(),
        JobKey::Game(dir) => dir
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        JobKey::Fsr411 => "fsr411".to_owned(),
    }
}

async fn log_lines(log: &mut Option<RunLog>, batch: &[Event], kind: JobKind, target: &str) {
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

fn trace_end(
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
async fn read_call_report(path: Option<&Path>) -> Option<CallReport> {
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
