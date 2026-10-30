//! A launch on its way to running ([`Prepared`]), the running result
//! ([`Session`]), and the sessions decanter started and hasn't seen end,
//! named by their [`Launch::key`]. How each step is done is the system's
//! own ([`crate::os`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Instant, SystemTime};

use anyhow::{Context as _, bail};
use futures_util::future::join_all;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::os::{self, Tracked};
use crate::{Launch, Prefix};

/// A launch with its command built but not started, so a caller can record
/// the command before it runs. [`Prepared::spawn`] starts it.
pub struct Prepared {
    /// What [`prepare`] built: `umu-run` under Proton, the program itself
    /// everywhere else.
    pub(crate) command: Command,
    /// What it was prepared from, for the spawn's own log lines.
    pub(crate) launch: Launch,
    /// What the command starts, named when the spawn fails.
    pub(crate) program: PathBuf,
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

    /// Starts the command, returning as soon as its process exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the command fails to spawn.
    #[inline]
    pub fn spawn(mut self) -> anyhow::Result<Session> {
        let child = match self
            .command
            .spawn()
            .with_context(|| format!("spawning {}", self.program.display()))
        {
            Ok(child) => child,
            Err(err) => {
                os::spawn_failed(&self.launch);
                return Err(err);
            }
        };
        let started_at = Instant::now();
        killed_lock().remove(&self.launch.key);
        if let Some(tracked) = os::spawned(&child, &self.launch) {
            live_lock().insert(self.launch.key.clone(), tracked);
        }
        Ok(Session {
            child,
            key: self.launch.key,
            prefix: self.launch.prefix,
            started_at,
        })
    }
}

/// A launched game.
///
/// Dropping this detaches rather than kills: the game keeps running, and
/// [`stop`] still reaches it by its key until it exits.
pub struct Session {
    /// The process decanter started: the game's own, or `umu-run` under
    /// Proton.
    pub(crate) child: Child,
    /// Taken out of the live set once the session ends.
    pub(crate) key: String,
    /// The launch's [`Launch::prefix`], which a session under Proton waits
    /// out once `umu-run` has exited.
    pub(crate) prefix: Option<Prefix>,
    /// When the child was spawned, so the exit's log line says how long the
    /// session ran without asking the OS for the child's own start time.
    pub(crate) started_at: Instant,
}

impl Session {
    /// The process decanter started. Under Proton that is `umu-run`, not
    /// the game: umu execs into the runtime container and Proton starts the
    /// game below that. Useful for logging and for [`Session::wait`], not
    /// as a handle on the game itself.
    #[inline]
    #[must_use]
    pub fn launcher_pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// The prefix the launch ran in, for a caller that stops or inspects it
    /// afterwards; `None` for a program that runs without one.
    #[inline]
    #[must_use]
    pub const fn prefix(&self) -> Option<&Prefix> {
        self.prefix.as_ref()
    }

    /// The launcher's pipes, once each, when the launch asked for
    /// [`Launch::capture_output`]; `None` otherwise. The game inherits
    /// them, so they stay open until the last process of the session is
    /// gone — read them to the end, or the game blocks on a full pipe.
    #[inline]
    pub const fn take_output(&mut self) -> (Option<ChildStdout>, Option<ChildStderr>) {
        (self.child.stdout.take(), self.child.stderr.take())
    }

    /// The launcher's stdin, once, when its command was given a piped one before
    /// [`Prepared::spawn`]; `None` otherwise. The game reads it until it is dropped.
    #[inline]
    pub const fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// Waits for the whole session: the process decanter started first,
    /// then anything it left running.
    ///
    /// A non-zero launcher exit is reported, but only after the session has
    /// ended — the games that fail this way have usually started something
    /// that is still up.
    ///
    /// # Errors
    ///
    /// Returns an error if the launcher can't be waited on, or if it
    /// exits with a non-zero status.
    #[inline]
    pub async fn wait(self) -> anyhow::Result<()> {
        let status = match self.wait_status().await {
            Ok(status) => status,
            Err(err) => return Err(err),
        };
        if !status.success() {
            bail!("{} exited with {status}", os::LAUNCHER);
        }
        Ok(())
    }

    /// [`Session::wait`] for a caller that records how the launcher ended
    /// rather than treating a non-zero exit as its own failure.
    ///
    /// Returns once every process the session started has exited too; the
    /// status is that of the process decanter started.
    ///
    /// # Errors
    ///
    /// Returns an error if the launcher can't be waited on.
    #[inline]
    pub async fn wait_status(self) -> anyhow::Result<ExitStatus> {
        os::wait_status(self).await
    }
}

/// Starts `launch`, returning as soon as its process is running.
///
/// # Errors
///
/// Returns an error if [`prepare`] does, or if the command fails to spawn.
#[inline]
pub async fn start(launch: Launch) -> anyhow::Result<Session> {
    match prepare(launch).await {
        Ok(prepared) => prepared.spawn(),
        Err(err) => Err(err),
    }
}

/// [`start`] up to the spawn: everything that can fail before the process
/// exists, with the command in hand for a caller to look at first.
///
/// # Errors
///
/// Returns an error if the executable isn't on disk (under Proton, unless
/// it is a bare command for Wine to resolve), has no directory to run in,
/// or, under Proton, off Linux, without a prefix or without `umu-run`.
#[inline]
pub async fn prepare(launch: Launch) -> anyhow::Result<Prepared> {
    os::prepare(launch).await
}

/// The sessions started and not yet seen end, by key.
///
/// Kept apart from whatever the UI is showing, because this is what
/// shutdown reads: a game outlives the launcher that started it — umu execs
/// into a container and Proton detaches, a launcher hands over to the game
/// and exits — so nothing stops on its own when decanter goes away.
/// Closing the app has to go and stop them.
fn live() -> &'static Mutex<HashMap<String, Tracked>> {
    static LIVE: OnceLock<Mutex<HashMap<String, Tracked>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// Locks [`live`], recovering a poisoned lock rather than discarding its
/// contents: a panic while holding it must not make [`stop_all`] believe no
/// games are running and leave them alive.
pub(crate) fn live_lock() -> MutexGuard<'static, HashMap<String, Tracked>> {
    live().lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sessions decanter had to kill, by key, with when it did; kept until [`take_killed`] reads it
/// or the key's next launch, because the status of a killed process reads like the game's own exit.
fn killed() -> &'static Mutex<HashMap<String, SystemTime>> {
    static KILLED: OnceLock<Mutex<HashMap<String, SystemTime>>> = OnceLock::new();
    KILLED.get_or_init(Default::default)
}

/// Locks [`killed`], recovering a poisoned lock for the same reason as [`live_lock`].
pub(crate) fn killed_lock() -> MutexGuard<'static, HashMap<String, SystemTime>> {
    killed().lock().unwrap_or_else(PoisonError::into_inner)
}

/// When decanter killed the session named `key`, if its last run ended that way rather than by
/// itself; clears the record.
#[inline]
#[must_use]
pub fn take_killed(key: &str) -> Option<SystemTime> {
    killed_lock().remove(key)
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
/// the game proper and exits. Asked politely first, so a game gets to save.
/// Returns once the session has ended, or at once when nothing runs under
/// `key`.
#[inline]
pub async fn stop(key: &str) {
    os::stop(key).await;
}

/// [`stop`] without the polite requests where the system can end a session
/// outright: for a player who asked for the game to go now rather than to
/// close. A game with a [`crate::Launch::stop_file`] is still asked through it
/// and given [`stop`]'s grace, since it ends at once and in order when asked so.
#[inline]
pub async fn end(key: &str) {
    os::end(key).await;
}

/// Stops every session at once, for shutdown: a game outlives decanter
/// otherwise, with nothing left to reach it from. Concurrent rather than one
/// after another, since each stop waits out its own session.
#[inline]
pub async fn stop_all() {
    let keys: Vec<String> = live_lock().keys().cloned().collect();
    if keys.is_empty() {
        return;
    }
    tracing::info!(count = keys.len(), "stopping running games");
    join_all(keys.iter().map(String::as_str).map(stop)).await;
}
