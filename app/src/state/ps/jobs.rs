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
use decanter_ps::run_log::{RunLog, describe_exit};
use decanter_ps::toolchain::Toolchain;
use gpui::{AppContext as _, AsyncApp, Entity, WeakEntity};
use rust_i18n::t;
use tokio::process::Command;

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
        JobKind::Update => t!("ps.jobs.update"),
        JobKind::Build => t!("ps.jobs.build"),
        JobKind::Game => t!("ps.jobs.game"),
        JobKind::Convert => t!("ps.jobs.convert"),
        JobKind::Fsr411 => t!("ps.jobs.fsr411"),
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

/// Replaces whatever job `key` held before; its log goes with it. `facts` go in the log's
/// header, before what `scanner` knows about the game.
#[expect(
    clippy::too_many_arguments,
    reason = "the library is the caller's, fixed when its job started"
)]
pub async fn run(
    ps: &WeakEntity<Ps>,
    root: &Root,
    key: JobKey,
    kind: JobKind,
    mut facts: Vec<(&'static str, String)>,
    mut command: Command,
    mut scanner: Option<Scanner>,
    cx: &mut AsyncApp,
) -> Finished {
    let target = match &key {
        JobKey::Port(port) => port.name().to_lowercase(),
        JobKey::Game(dir) => dir
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
        JobKey::Fsr411 => "fsr411".to_owned(),
    };
    facts.extend(scanner.as_ref().map(Scanner::facts).into_iter().flatten());
    // A job runs without its file rather than not at all; the log view says so.
    let (mut log, log_error) = match RunLog::start(root, &target, kind, &facts, &mut command, scanner.is_some()).await {
        Ok(log) => (Some(log), None),
        Err(err) => {
            tracing::warn!(job = kind.name(), target, %err, "could not create the job's log file");
            (None, Some(t!("ps.jobs.log_error", error = err).into_owned()))
        }
    };
    let program = command.as_std().get_program().to_string_lossy().into_owned();
    let (mut job, mut events) = match process::spawn(kind, command) {
        Ok(spawned) => spawned,
        Err(err) => {
            tracing::warn!(job = kind.name(), target, program, %err, "could not start job");
            if let Some(mut file) = log {
                let written = file.line(&format!("# error: could not start {program}: {err}")).await;
                if let Err(err) = written.and(file.finish(None, None).await) {
                    tracing::warn!(job = kind.name(), target, %err, "could not finish the job's log file");
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
            };
        }
    };
    let log_path = log.as_ref().map(|log| log.path().to_owned());
    let calls_path = log.as_ref().and_then(|log| log.calls_path().map(Path::to_owned));
    tracing::info!(job = kind.name(), target, program, log = ?log_path, "job started");
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
        };
    }
    let mut batch = Vec::with_capacity(BATCH);
    let mut exit = None;
    while events.recv_many(&mut batch, BATCH).await > 0 {
        if let Some(file) = log.as_mut() {
            for event in &batch {
                if let Event::Line(line) = event
                    && let Err(err) = file.line(line).await
                {
                    tracing::warn!(job = kind.name(), target, %err, "the job's log file stopped taking lines");
                    log = None;
                    break;
                }
            }
        }
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
    if let Some(file) = log
        && let Err(err) = file.finish(exit, report.as_ref()).await
    {
        tracing::warn!(job = kind.name(), target, %err, "could not finish the job's log file");
    }
    let stopped = job.read_with(cx, |job, _| job.stop_requested);
    trace_end(kind, &target, exit, stopped, started, log_path.as_deref());
    let code = exit.and_then(|status| status.code());
    let _ = ps.update(cx, |_, cx| cx.notify());
    let failures = scanner.map_or_else(Vec::new, |scanner| {
        scanner.exit(code, stopped, log_path.as_deref(), report)
    });
    Finished { code, failures }
}

fn trace_end(
    kind: JobKind,
    target: &str,
    exit: Option<ExitStatus>,
    stopped: bool,
    started: Instant,
    log: Option<&Path>,
) {
    let duration_ms = started.elapsed().as_millis();
    let succeeded = exit.is_some_and(|status| status.success());
    let exit = describe_exit(exit);
    if succeeded || stopped {
        tracing::info!(
            job = kind.name(),
            target,
            exit,
            stopped,
            duration_ms,
            ?log,
            "job finished"
        );
    } else {
        tracing::warn!(job = kind.name(), target, exit, duration_ms, ?log, "job failed");
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
