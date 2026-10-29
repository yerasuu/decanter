//! Running a port's process to completion with its output in an entity of
//! its own, so a chatty game redraws only the log that shows it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Instant;

use decanter_ps::failures::Scanner;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::{Event, Job, JobKind};
use decanter_ps::run_log::{self, RunLog};
use gpui::{AppContext as _, AsyncApp, Entity, WeakEntity};
use rust_i18n::t;
use tokio::process::Command;

pub use crate::ps_play::run::{Finished, Launcher, port_facts};
use crate::ps_play::run::{finish_log, finished, game_target, log_lines, read_call_report, trace_end};

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
            return Finished::none();
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
        return Finished::none();
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

/// What a job's log files are named after ([`RunLog::start`]'s `target`).
pub fn target_name(key: &JobKey) -> String {
    match key {
        JobKey::Port(port) => port.name().to_lowercase(),
        JobKey::Game(dir) => game_target(dir),
        JobKey::Fsr411 => "fsr411".to_owned(),
    }
}
