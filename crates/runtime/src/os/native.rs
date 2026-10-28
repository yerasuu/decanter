//! Running a game as the program it already is on this system (a Windows
//! program on Windows, a macOS one on macOS): its own executable, started
//! directly, with nothing in between.
//!
//! Sessions are named by a caller's key rather than by anything on disk,
//! since there is no prefix to scan for what a game left running: the
//! session holds the process decanter started and every process started
//! from it (a Job Object on Windows, a process group on macOS), so it lasts
//! until the last of them exits (a launcher that starts the game and leaves
//! doesn't end it), and stopping it ends all of them.

#![expect(
    clippy::single_call_fn,
    reason = "helpers are split out to name a step of a session, not because they're reused"
)]

use core::time::Duration;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::SystemTime;

use anyhow::{Context as _, bail};
use tokio::process::Child;
use tokio::time::sleep;

use super::platform as os;
use crate::session::{killed_lock, live_lock};
use crate::{Launch, Prefix, Prepared, Session, is_live};
pub(crate) use os::Tracked;

/// How often [`crate::stop`] asks a session's processes to close again.
const STOP_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How often a session whose own process has exited checks whether anything
/// it started still runs.
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Polite requests [`crate::stop`] makes before ending the tree outright, so a
/// game gets a few seconds to write what it hadn't yet.
const POLITE_STOP_ATTEMPTS: u32 = 5;

/// What is left of a session once the process decanter started has exited.
pub(super) enum Remaining {
    /// Nothing, or nothing decanter can still follow.
    Done,
    /// How many processes, where the system says.
    Running(Option<usize>),
}

/// What the session's process is called in its exit's log lines.
pub(crate) const LAUNCHER: &str = "the game";

/// The executable itself, which must exist as a file, run from its own
/// directory unless the launch names another.
pub(crate) async fn prepare(mut launch: Launch) -> anyhow::Result<Prepared> {
    if !launch.executable.is_file() {
        tracing::warn!(executable = %launch.executable.display(), key = %launch.key, "rejecting launch: no such executable on disk");
        bail!("no executable at {}", launch.executable.display());
    }
    let working_directory = match launch.working_directory.take() {
        Some(directory) => directory,
        None => match launch
            .executable
            .parent()
            .map(Path::to_path_buf)
            .context("executable has no parent directory")
        {
            Ok(parent) => parent,
            Err(err) => return Err(err),
        },
    };

    let mut command = os::command(&launch.executable);
    command
        .args(&launch.args)
        .current_dir(&working_directory)
        .envs(launch.env.iter().map(|entry| (entry.0.as_str(), entry.1.as_str())))
        .stdin(Stdio::null());
    if launch.capture_output {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        os::hide_console(&mut command);
    }

    // `launch.env` isn't logged: it has no fixed shape, so nothing rules out
    // a caller routing a secret through it.
    tracing::debug!(
        executable = %launch.executable.display(),
        args = ?launch.args,
        working_directory = %working_directory.display(),
        key = %launch.key,
        "native command built"
    );
    Ok(Prepared {
        command,
        program: launch.executable.clone(),
        launch,
    })
}

pub(crate) fn spawn_failed(launch: &Launch) {
    tracing::error!(executable = %launch.executable.display(), key = %launch.key, "failed to spawn the game");
}

/// The process decanter started and every process it starts, followed
/// until they have all exited.
pub(crate) fn spawned(child: &Child, launch: &Launch) -> Option<Tracked> {
    let spawned = child.id();
    tracing::info!(pid = spawned, key = %launch.key, "launched game process");
    spawned.map(|pid| Tracked::new(child, pid, &launch.key))
}

/// The game's own exit, then that of every process it started, because a
/// launcher that started the game and exited hasn't ended the session.
pub(crate) async fn wait_status(mut session: Session) -> anyhow::Result<ExitStatus> {
    let pid = session.child.id();
    let waited = session.child.wait().await.context("waiting for the game");
    let status = match waited {
        Ok(status) => status,
        Err(err) => {
            live_lock().remove(&session.key);
            tracing::error!(pid, key = %session.key, "failed to wait for the game");
            return Err(err);
        }
    };
    outlive_launcher(&session.key, status).await;
    live_lock().remove(&session.key);
    let elapsed = session.started_at.elapsed();
    let killed_at = killed_lock().get(&session.key).copied();
    if let Some(killed_at) = killed_at {
        tracing::warn!(
            pid,
            exit_code = %format_args!("{:#010X}", crate::KILLED_EXIT_CODE),
            process_exit_code = status.code(),
            elapsed = ?elapsed,
            stop_requested_at = ?killed_at,
            "game ended by decanter killing it; its exit code is not the game's own"
        );
    } else if status.success() {
        tracing::info!(pid, exit_code = status.code(), elapsed = ?elapsed, "game exited");
    } else {
        tracing::error!(pid, exit_code = status.code(), elapsed = ?elapsed, "game exited with a failure status");
    }
    Ok(status)
}

/// Waits until nothing in the session named `key` runs any more, because a
/// launcher that started the game and exited hasn't ended it.
async fn outlive_launcher(key: &str, launcher_status: ExitStatus) {
    let mut is_announced = false;
    loop {
        let remaining = live_lock().get(key).map(|tracked| tracked.remaining(key));
        match remaining {
            None | Some(Remaining::Done) => return,
            Some(Remaining::Running(count)) => {
                if !is_announced {
                    tracing::info!(
                        key,
                        launcher_exit_code = launcher_status.code(),
                        remaining = count,
                        "launched process exited; following the processes it started"
                    );
                    is_announced = true;
                }
            }
        }
        sleep(SESSION_POLL_INTERVAL).await;
    }
}

/// Asked politely first, so a game gets to save, and forced only after
/// [`POLITE_STOP_ATTEMPTS`].
pub(crate) async fn stop(key: &str) {
    end_after(key, POLITE_STOP_ATTEMPTS).await;
}

pub(crate) async fn end(key: &str) {
    end_after(key, 0).await;
}

/// Ends the session after `polite_attempts` requests to close, each a
/// [`STOP_POLL_INTERVAL`] apart.
async fn end_after(key: &str, polite_attempts: u32) {
    tracing::debug!(key, polite_attempts, "stopping session");
    for attempt in 0..=polite_attempts {
        let is_forced = attempt == polite_attempts;
        let reached = live_lock().get_mut(key).map(|tracked| {
            if is_forced {
                tracked.release()
            } else {
                tracked.targets()
            }
        });
        let Some(targets) = reached else {
            tracing::debug!(key, "session has exited");
            return;
        };
        if is_forced {
            killed_lock().insert(key.to_owned(), SystemTime::now());
            tracing::warn!(key, "session did not close by itself; killing it");
        }
        os::close(targets, is_forced).await;
        sleep(STOP_POLL_INTERVAL).await;
    }
    if is_live(key) {
        tracing::warn!(key, "session still running after being ended");
    }
}

/// No prefix has anything running in it here: games run as themselves.
pub(crate) const fn prefix_processes(_: &Prefix) -> Vec<u32> {
    Vec::new()
}

/// Every prefix asked about, each with nothing running in it.
pub(crate) fn processes_by_prefix(prefixes: &[Prefix]) -> HashMap<PathBuf, Vec<u32>> {
    prefixes
        .iter()
        .map(|prefix| (prefix.path.clone(), Vec::new()))
        .collect()
}

/// Nothing to stop: nothing runs in a prefix here.
pub(crate) async fn stop_prefix(_: &Prefix) {}

pub(crate) const fn live_prefixes() -> Vec<Prefix> {
    Vec::new()
}

/// No Steam compatibility tools folder to look in: Proton runs only on Linux.
pub(crate) const fn steam_compat_dir() -> Option<PathBuf> {
    None
}
