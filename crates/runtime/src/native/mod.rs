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
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Instant, SystemTime};

use anyhow::{Context as _, bail};
use futures_util::future::join_all;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::time::sleep;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as os;
use os::Tracked;
#[cfg(windows)]
use windows as os;

/// How often [`stop`] asks a session's processes to close again.
const STOP_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// How often a session whose own process has exited checks whether anything
/// it started still runs.
const SESSION_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Polite requests [`stop`] makes before ending the tree outright, so a
/// game gets a few seconds to write what it hadn't yet.
const POLITE_STOP_ATTEMPTS: u32 = 5;

/// What is left of a session once the process decanter started has exited.
enum Remaining {
    /// Nothing, or nothing decanter can still follow.
    Done,
    /// How many processes, where the system says.
    Running(Option<usize>),
}

/// One game launch.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Launch {
    pub args: Vec<String>,
    /// Pipe the game's stdout and stderr to the caller
    /// ([`Session::take_output`]) instead of inheriting decanter's.
    pub capture_output: bool,
    /// Extra environment on top of decanter's own.
    pub env: Vec<(String, String)>,
    /// Must exist as a file: [`prepare`] rejects anything else.
    pub executable: PathBuf,
    /// Names the session for [`stop`] and [`is_live`]; one live session
    /// per key.
    pub key: String,
    /// Defaults to the executable's own directory, which is where most
    /// games load their data from by relative path.
    pub working_directory: Option<PathBuf>,
}

impl Launch {
    #[inline]
    pub fn new<P, K>(executable: P, key: K) -> Self
    where
        P: Into<PathBuf>,
        K: Into<String>,
    {
        Self {
            args: Vec::new(),
            capture_output: false,
            env: Vec::new(),
            executable: executable.into(),
            key: key.into(),
            working_directory: None,
        }
    }
}

/// A launched game.
///
/// Dropping this detaches rather than kills: the game keeps running, and
/// [`stop`] still reaches it by its key until it exits.
pub struct Session {
    /// The game's own process.
    child: Child,
    /// Taken out of the live set once the session ends.
    key: String,
    /// When the child was spawned, for the exit's log line.
    started_at: Instant,
}

impl Session {
    #[inline]
    #[must_use]
    pub fn launcher_pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// The game's pipes, once each, when the launch asked for
    /// [`Launch::capture_output`]; `None` otherwise. Read them to the end,
    /// or the game blocks on a full pipe.
    #[inline]
    pub const fn take_output(&mut self) -> (Option<ChildStdout>, Option<ChildStderr>) {
        (self.child.stdout.take(), self.child.stderr.take())
    }

    /// Waits for the game to exit.
    ///
    /// # Errors
    ///
    /// Returns an error if the process can't be waited on, or if it exits
    /// with a non-zero status.
    #[inline]
    pub async fn wait(self) -> anyhow::Result<()> {
        let status = match self.wait_status().await {
            Ok(status) => status,
            Err(err) => return Err(err),
        };
        if !status.success() {
            bail!("the game exited with {status}");
        }
        Ok(())
    }

    /// [`Session::wait`] for a caller that records how the game ended
    /// rather than treating a non-zero exit as its own failure.
    ///
    /// Returns once every process the game started has exited too; the
    /// status is that of the process decanter started.
    ///
    /// # Errors
    ///
    /// Returns an error if the process can't be waited on.
    #[inline]
    pub async fn wait_status(mut self) -> anyhow::Result<ExitStatus> {
        let pid = self.child.id();
        let waited = self.child.wait().await.context("waiting for the game");
        let status = match waited {
            Ok(status) => status,
            Err(err) => {
                live_lock().remove(&self.key);
                tracing::error!(pid, key = %self.key, "failed to wait for the game");
                return Err(err);
            }
        };
        outlive_launcher(&self.key, status).await;
        live_lock().remove(&self.key);
        let elapsed = self.started_at.elapsed();
        let killed_at = killed_lock().get(&self.key).copied();
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
}

/// A launch with its command built but not started, so a caller can record
/// the command before it runs. [`Prepared::spawn`] starts it.
pub struct Prepared {
    /// The game's executable with everything [`prepare`] put on it.
    command: Command,
    /// What it was prepared from, for the spawn's own log lines.
    launch: Launch,
}

impl Prepared {
    /// The command as it will be spawned: program, arguments, working
    /// directory and the environment decanter sets on top of its own.
    #[inline]
    pub const fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    /// The session's key, for [`take_killed`].
    #[inline]
    #[must_use]
    pub fn key(&self) -> &str {
        &self.launch.key
    }

    /// Starts the game, returning as soon as its process exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the executable fails to spawn.
    #[inline]
    pub fn spawn(mut self) -> anyhow::Result<Session> {
        let child = match self
            .command
            .spawn()
            .with_context(|| format!("spawning {}", self.launch.executable.display()))
        {
            Ok(child) => child,
            Err(err) => {
                tracing::error!(executable = %self.launch.executable.display(), key = %self.launch.key, "failed to spawn the game");
                return Err(err);
            }
        };
        let spawned = child.id();
        tracing::info!(pid = spawned, key = %self.launch.key, "launched game process");
        killed_lock().remove(&self.launch.key);
        if let Some(pid) = spawned {
            let tracked = Tracked::new(&child, pid, &self.launch.key);
            live_lock().insert(self.launch.key.clone(), tracked);
        }
        Ok(Session {
            child,
            key: self.launch.key,
            started_at: Instant::now(),
        })
    }
}

/// Starts `launch`, returning as soon as the game is running.
///
/// # Errors
///
/// Returns an error if the executable isn't on disk, has no directory to
/// run in, or fails to spawn.
#[inline]
pub fn start(launch: Launch) -> anyhow::Result<Session> {
    match prepare(launch) {
        Ok(prepared) => prepared.spawn(),
        Err(err) => Err(err),
    }
}

/// [`start`] up to the spawn: everything that can fail before the process
/// exists, with the command in hand for a caller to look at first.
///
/// # Errors
///
/// Returns an error if the executable isn't on disk or has no directory to
/// run in.
#[inline]
pub fn prepare(mut launch: Launch) -> anyhow::Result<Prepared> {
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
        #[cfg(windows)]
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
    Ok(Prepared { command, launch })
}

/// The sessions started and not yet seen end, by key.
fn live() -> &'static Mutex<HashMap<String, Tracked>> {
    static LIVE: OnceLock<Mutex<HashMap<String, Tracked>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// Locks [`live`], recovering a poisoned lock rather than discarding its
/// contents: a panic while holding it must not make [`stop_all`] believe no
/// games are running and leave them alive.
fn live_lock() -> MutexGuard<'static, HashMap<String, Tracked>> {
    live().lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sessions [`end_after`] had to kill, by key, with when it did; kept until [`take_killed`] reads it
/// or the key's next launch, because the status of a killed process reads like the game's own exit.
fn killed() -> &'static Mutex<HashMap<String, SystemTime>> {
    static KILLED: OnceLock<Mutex<HashMap<String, SystemTime>>> = OnceLock::new();
    KILLED.get_or_init(Default::default)
}

fn killed_lock() -> MutexGuard<'static, HashMap<String, SystemTime>> {
    killed().lock().unwrap_or_else(PoisonError::into_inner)
}

/// When decanter killed the session named `key`, if its last run ended that way rather than by
/// itself; clears the record.
#[inline]
#[must_use]
pub fn take_killed(key: &str) -> Option<SystemTime> {
    killed_lock().remove(key)
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

/// Whether the session named `key` is still running.
#[inline]
#[must_use]
pub fn is_live(key: &str) -> bool {
    live_lock().contains_key(key)
}

/// Asks the session named `key` to close, then ends it if it won't.
///
/// Every process in the session, because a game's launcher often starts
/// the game proper and exits. Asked politely first, so a game gets to save,
/// and forced only after [`POLITE_STOP_ATTEMPTS`]. Returns once the session
/// has ended, or at once when nothing runs under `key`.
#[inline]
pub async fn stop(key: &str) {
    end_after(key, POLITE_STOP_ATTEMPTS).await;
}

/// [`stop`] without the polite requests: the session is ended at once, for
/// a player who asked for the game to go now rather than to close.
#[inline]
pub async fn end(key: &str) {
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

/// Stops every session at once, for shutdown: a game outlives decanter
/// otherwise, with nothing left to reach it from.
#[inline]
pub async fn stop_all() {
    let keys: Vec<String> = live_lock().keys().cloned().collect();
    if keys.is_empty() {
        return;
    }
    tracing::info!(count = keys.len(), "stopping running games");
    join_all(keys.iter().map(String::as_str).map(stop)).await;
}
