//! Windows: a session is a Job Object holding the process decanter started
//! and every process started from it, and a stop is `taskkill` over them.

use std::path::Path;
use std::process::Stdio;

use futures_util::future::join_all;
use tokio::process::{Child, Command};
use win32job::Job;

use super::Remaining;

/// `CREATE_NO_WINDOW`, so `taskkill` doesn't flash a console over the game.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What a live session's processes are found by.
pub(super) struct Tracked {
    /// `None` when the game couldn't be put in one: the session is then
    /// `pid` and its tree, as `taskkill /T` finds it.
    job: Option<Job>,
    /// The process decanter started.
    pid: u32,
}

/// The processes a stop asks to close.
pub(super) type Targets = Vec<u32>;

impl Tracked {
    /// Puts `child` in a job of its own, or follows it alone when Windows won't.
    pub(super) fn new(child: &Child, pid: u32, key: &str) -> Self {
        Self {
            job: join_job(child, key),
            pid,
        }
    }

    /// What still runs in the job once the process decanter started has
    /// exited; without a job, nothing is followed past it.
    pub(super) fn remaining(&self, key: &str) -> Remaining {
        let Some(job) = self.job.as_ref() else {
            return Remaining::Done;
        };
        match job.query_process_id_list() {
            Ok(pids) if pids.is_empty() => Remaining::Done,
            Ok(pids) => Remaining::Running(Some(pids.len())),
            Err(err) => {
                tracing::warn!(key, error = ?err, "cannot list the session's processes; ending the session");
                Remaining::Done
            }
        }
    }

    /// The session's processes now, or the one decanter started when the
    /// job can't be listed, so a stop still reaches the tree it heads.
    pub(super) fn targets(&self) -> Targets {
        let Some(job) = self.job.as_ref() else {
            return vec![self.pid];
        };
        match job.query_process_id_list() {
            Ok(pids) => pids.into_iter().filter_map(|pid| u32::try_from(pid).ok()).collect(),
            Err(err) => {
                tracing::warn!(pid = self.pid, error = ?err, "cannot list the session's processes");
                vec![self.pid]
            }
        }
    }
}

/// The game's executable as a command; nothing to set up before the spawn,
/// since the job is joined right after it.
pub(super) fn command(executable: &Path) -> Command {
    Command::new(executable)
}

/// Asks the processes and their children to close (`taskkill` without
/// `/F` sends their windows a close), or ends them when `force`d.
pub(super) async fn close(targets: Targets, force: bool) {
    join_all(targets.into_iter().map(|pid| taskkill(pid, force))).await;
}

/// A job holding `child` and, from then on, every process it starts, or
/// `None` to follow `child` alone when Windows won't make one.
///
/// The job doesn't kill on close, so that a game outlives a decanter that
/// crashed, as it does on Linux.
// NOTE: `child` runs for a moment before it joins, and a process it starts
// in that window escapes the job; closing it needs the child created
// suspended and its thread resumed, which std's `Command` has no safe way to do.
fn join_job(child: &Child, key: &str) -> Option<Job> {
    let Some(handle) = child
        .raw_handle()
        .and_then(|handle| isize::try_from(handle.addr()).ok())
    else {
        tracing::warn!(
            key,
            "the game has no process handle; following the launched process alone"
        );
        return None;
    };
    let job = match Job::create() {
        Ok(job) => job,
        Err(err) => {
            tracing::warn!(key, error = ?err, "cannot create a job object; following the launched process alone");
            return None;
        }
    };
    if let Err(err) = job.assign_process(handle) {
        tracing::warn!(key, error = ?err, "cannot assign the game to its job object; following the launched process alone");
        return None;
    }
    Some(job)
}

/// Asks `pid` and its children to close, or ends them when `force`d. Racy
/// by nature: the process may already be gone, which is the outcome being
/// asked for anyway, so a failure is traced rather than warned about.
async fn taskkill(pid: u32, force: bool) {
    let mut command = Command::new("taskkill");
    command.arg("/PID").arg(pid.to_string()).arg("/T");
    if force {
        command.arg("/F");
    }
    let status = command
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    tracing::trace!(pid, force, ?status, "ran taskkill");
}
