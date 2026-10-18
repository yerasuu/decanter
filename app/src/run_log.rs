//! A PC game's run recorded the way a PS job is: its whole output in a file
//! under the library's `decanter/logs/`, beside decanter's own, and its
//! end in the [`Job`] the library's Registro tab follows.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use decanter_ps::process::{Event, Job, JobKind};
use decanter_ps::run_log::RunLog;
use decanter_runtime::Prepared;
use gpui::{AsyncApp, Entity};
use tokio::sync::mpsc::UnboundedReceiver;

/// Lines taken per redraw: a burst of output costs one notify, not one per line.
const BATCH: usize = 256;

fn dir(library_root: &Path) -> PathBuf {
    decanter_core::bootstrap::library_data_dir(library_root).join("logs")
}

/// Before `prepared` is spawned, so the header holds the command it runs.
pub async fn start(
    account_id: &str,
    game_id: &str,
    facts: &[(&str, String)],
    prepared: &mut Prepared,
) -> Result<RunLog, String> {
    let library = crate::core::library_for_account(account_id).map_err(|err| err.to_string())?;
    RunLog::start(
        &dir(library.root()),
        game_id,
        JobKind::Game,
        facts,
        prepared.command_mut(),
        false,
    )
    .await
    .map_err(|err| err.to_string())
}

/// Ends the log of a run whose launcher never started, with why.
pub async fn abandon(mut log: RunLog, game_id: &str, launcher: &str, reason: &str) {
    let path = log.path().to_owned();
    let written = log
        .line(&format!("# error: could not start {launcher}: {reason}"))
        .await;
    match written.and(log.finish(None, None).await) {
        Ok(()) => crate::debug_logs::keep_job(path, None).await,
        Err(err) => tracing::warn!(game_id, %err, "could not finish the run's log file"),
    }
}

/// Feeds `events` to the file and to `job` until the run ends; how it ended,
/// with the file left open until [`finish`], so the backend can still add to it.
pub async fn follow(
    job: &Entity<Job>,
    mut events: UnboundedReceiver<Event>,
    mut log: Option<RunLog>,
    game_id: &str,
    cx: &mut AsyncApp,
) -> (Option<ExitStatus>, Option<RunLog>) {
    let mut batch = Vec::with_capacity(BATCH);
    let mut exit = None;
    while events.recv_many(&mut batch, BATCH).await > 0 {
        if let Some(file) = log.as_mut() {
            for event in &batch {
                if let Event::Line(line) = event
                    && let Err(err) = file.line(line).await
                {
                    tracing::warn!(game_id, %err, "the run's log file stopped taking lines");
                    log = None;
                    break;
                }
            }
        }
        job.update(cx, |job, cx| {
            for event in batch.drain(..) {
                if let Event::Exited(status) = &event {
                    exit = *status;
                }
                job.push(event);
            }
            cx.notify();
        });
    }
    (exit, log)
}

/// Adds what decanter makes of a run's end: `file_line` to its file, in the
/// log's English, and `job_line` to the Registro tab, in the UI's language.
#[cfg(not(proton))]
pub async fn note(
    job: &Entity<Job>,
    mut log: Option<RunLog>,
    file_line: &str,
    job_line: String,
    game_id: &str,
    cx: &mut AsyncApp,
) -> Option<RunLog> {
    if let Some(file) = log.as_mut()
        && let Err(err) = file.line(file_line).await
    {
        tracing::warn!(game_id, %err, "the run's log file stopped taking lines");
        log = None;
    }
    job.update(cx, |job, cx| {
        job.push(Event::Line(job_line));
        cx.notify();
    });
    log
}

pub async fn finish(log: Option<RunLog>, exit: Option<ExitStatus>, game_id: &str) {
    if let Some(file) = log {
        let path = file.path().to_owned();
        match file.finish(exit, None).await {
            Ok(()) => crate::debug_logs::keep_job(path, None).await,
            Err(err) => tracing::warn!(game_id, %err, "could not finish the run's log file"),
        }
    }
}
