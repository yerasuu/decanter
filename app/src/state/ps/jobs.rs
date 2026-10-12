//! Running a port's process to completion with its output in an entity of
//! its own, so a chatty game redraws only the log that shows it.

use std::path::PathBuf;

use decanter_ps::failures::{Failure, Scanner};
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::{self, Event, Job};
use decanter_ps::run_log::RunLog;
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

/// Replaces whatever job `key` held before; its log goes with it.
#[expect(
    clippy::too_many_arguments,
    reason = "the library is the caller's, fixed when its job started"
)]
pub async fn run(
    ps: &WeakEntity<Ps>,
    root: &Root,
    key: JobKey,
    label: &str,
    command: Command,
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
    let facts = scanner.as_ref().map(Scanner::facts).unwrap_or_default();
    // A job runs without its file rather than not at all; the log view says so.
    let (mut log, log_error) = match RunLog::start(root, &target, label, &facts, &command).await {
        Ok(log) => (Some(log), None),
        Err(err) => {
            tracing::warn!(label, %err, "could not create the job's log file");
            (None, Some(t!("ps.jobs.log_error", error = err).into_owned()))
        }
    };
    let (mut job, mut events) = match process::spawn(label, command) {
        Ok(spawned) => spawned,
        Err(err) => {
            tracing::warn!(label, %err, "could not start job");
            if let Some(mut file) = log {
                let written = file.line(&format!("could not start: {err}")).await;
                if let Err(err) = written.and(file.finish(None).await) {
                    tracing::warn!(label, %err, "could not finish the job's log file");
                }
            }
            let job = cx.new(|_| Job::failed(label, t!("ps.jobs.start_error", error = err).into_owned()));
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
    job.log_file = log.as_ref().map(|log| log.path().to_owned());
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
    let mut code = None;
    while events.recv_many(&mut batch, BATCH).await > 0 {
        if let Some(file) = log.as_mut() {
            for event in &batch {
                if let Event::Line(line) = event
                    && let Err(err) = file.line(line).await
                {
                    tracing::warn!(label, %err, "the job's log file stopped taking lines");
                    log = None;
                    break;
                }
            }
        }
        job.update(cx, |job, cx| {
            for event in batch.drain(..) {
                match (&event, scanner.as_mut()) {
                    (Event::Line(line), Some(scanner)) => scanner.line(line),
                    (Event::Exited(exit), _) => code = *exit,
                    (Event::Line(_), None) => {}
                }
                job.push(event);
            }
            cx.notify();
        });
    }
    if let Some(file) = log
        && let Err(err) = file.finish(code).await
    {
        tracing::warn!(label, %err, "could not finish the job's log file");
    }
    let stopped = job.read_with(cx, |job, _| job.stop_requested);
    let _ = ps.update(cx, |_, cx| cx.notify());
    Finished {
        code,
        failures: scanner.map(|scanner| scanner.exit(code, stopped)).unwrap_or_default(),
    }
}
