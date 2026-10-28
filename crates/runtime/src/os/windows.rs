//! Windows: a session is a Job Object holding the process decanter started
//! and every process started from it, and a stop is `taskkill` over them,
//! ending with the job itself.

#![expect(
    clippy::single_call_fn,
    reason = "helpers are split out to name a step of a session, not because they're reused"
)]

use std::path::Path;
use std::process::Stdio;

use futures_util::future::join_all;
use tokio::process::{Child, Command};
use win32job::{ExtendedLimitInfo, Job};

use super::native::Remaining;

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

/// What a stop reaches.
pub(super) struct Targets {
    /// Only on the forced stop, which closes it to end everything in it.
    job: Option<Job>,
    /// The processes asked to close, or ended when forced.
    pids: Vec<u32>,
}

impl Tracked {
    /// Puts `child` in a job of its own, or follows it alone when Windows won't.
    pub(super) fn new(child: &Child, pid: u32, key: &str) -> Self {
        Self {
            job: join_job(child, key),
            pid,
        }
    }

    /// The session's processes now, or the one decanter started when the
    /// job can't be listed, so a stop still reaches the tree it heads.
    fn pids(&self) -> Vec<u32> {
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

    /// [`Tracked::targets`] for the forced stop, with the job handed over so
    /// closing it ends the session whatever `taskkill` manages. The session
    /// is no longer followed past the process decanter started, since nothing
    /// is left in a job once it is closed.
    pub(super) fn release(&mut self) -> Targets {
        let pids = self.pids();
        Targets {
            job: self.job.take(),
            pids,
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

    /// The session's processes now, for a stop that asks them to close.
    pub(super) fn targets(&self) -> Targets {
        Targets {
            job: None,
            pids: self.pids(),
        }
    }
}

/// The game's executable as a command; nothing to set up before the spawn,
/// since the job is joined right after it.
pub(super) fn command(executable: &Path) -> Command {
    Command::new(executable)
}

/// Keeps a console-subsystem game from opening a console window of its own
/// when decanter reads its output instead, since Windows Terminal would
/// show it as a tab over decanter.
pub(super) fn hide_console(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

/// Asks the processes and their children to close (`taskkill` without
/// `/F` sends their windows a close), or ends them when `force`d.
pub(super) async fn close(targets: Targets, force: bool) {
    if let Some(job) = targets.job {
        terminate(job);
    }
    join_all(targets.pids.into_iter().map(|pid| taskkill(pid, force))).await;
}

/// Ends every process in `job` by closing decanter's only handle to it once
/// it kills on close
/// ([`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)),
/// because a forced `taskkill` was seen leaving a game running (#943).
fn terminate(job: Job) {
    let mut limits = ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    match job.set_extended_limit_info(&limits) {
        Ok(()) => tracing::debug!("ending the session's job"),
        Err(err) => {
            tracing::warn!(error = ?err, "cannot make the session's job end its processes; relying on taskkill");
        }
    }
    drop(job);
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
