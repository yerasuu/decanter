//! A Proton prefix: where it is, where a program's stored Windows path lands
//! in it, and what runs in it, which only Linux has anything of.

use core::time::Duration;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context as _;
use tokio::time::sleep;

use crate::os;
use crate::path::sanitize_relative_path;
use crate::{Launch, Proton};

/// How often [`Prefix::wait_until_idle`] re-checks for leftover processes.
/// Only reached after the launcher already exited, so it costs one
/// directory scan per tick and nothing while a game is actually running.
pub(crate) const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A Proton prefix decanter owns.
///
/// umu treats `WINEPREFIX` as Proton's `STEAM_COMPAT_DATA_PATH` and then
/// symlinks `pfx` inside it back to the directory itself, so — unlike a
/// Steam `compatdata` layout — `drive_c` sits directly under this path.
#[derive(Debug, Clone)]
pub struct Prefix {
    /// Filesystem path to the prefix root.
    pub(crate) path: PathBuf,
}

impl Prefix {
    /// Where games get installed. Created on demand — umu creates it on
    /// first launch, but an install has to write files there before any
    /// launch has happened.
    #[inline]
    #[must_use]
    pub fn drive_c(&self) -> PathBuf {
        self.path.join("drive_c")
    }

    /// Creates the prefix if it isn't there yet, and returns once Wine has
    /// finished laying it out.
    ///
    /// Only needed before something other than a launch touches the
    /// prefix — an install writing into `drive_c`, most of all. A plain
    /// [`crate::start`] into a missing prefix creates it on the way.
    ///
    /// # Errors
    ///
    /// Returns an error if `wineboot` fails to spawn or exits with a
    /// non-zero status.
    #[inline]
    pub async fn initialize(&self, proton: Proton) -> anyhow::Result<()> {
        tracing::debug!(prefix = %self.path.display(), ?proton, "initializing prefix with wineboot");
        // `wineboot`, not a path to it: on a prefix that doesn't exist
        // there is nothing to point at yet, so Wine resolves it from the
        // Proton build instead.
        let mut launch = Launch::new("wineboot", self.path.display().to_string())
            .game_id("umu-decanter")
            .prefix(self.clone());
        launch.args.push("-i".to_owned());
        launch.proton = proton;
        launch.working_directory = Some(env::temp_dir());
        let session = match crate::start(launch).await {
            Ok(session) => session,
            Err(err) => return Err(err),
        };
        session.wait().await.context("initializing the prefix with wineboot")
    }

    #[inline]
    pub fn new<P>(path: P) -> Self
    where
        P: Into<PathBuf>,
    {
        Self { path: path.into() }
    }

    #[inline]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Pids of every live process running against this prefix, the
    /// launcher's own children included; none off Linux, where nothing runs
    /// in a prefix.
    #[inline]
    #[must_use]
    pub fn processes(&self) -> Vec<u32> {
        os::prefix_processes(self)
    }

    /// Resolves a Windows path as stored on a program (`C:\Games\x.exe`)
    /// to the host path umu should be handed.
    ///
    /// Only `C:` resolves: that's the only drive decanter ever creates a
    /// program under, and mapping an arbitrary drive letter would mean
    /// reading `dosdevices`, which is the prefix's business, not ours.
    #[inline]
    #[must_use]
    pub fn resolve_windows_path(&self, windows_path: &str) -> Option<PathBuf> {
        let Some(rest) = windows_path
            .strip_prefix("C:\\")
            .or_else(|| windows_path.strip_prefix("c:\\"))
        else {
            tracing::warn!(windows_path, "rejecting windows path: not on the C: drive");
            return None;
        };
        let Some(relative) = sanitize_relative_path(&rest.replace('\\', "/")) else {
            tracing::warn!(windows_path, "rejecting windows path: escapes the prefix");
            return None;
        };
        Some(self.drive_c().join(relative))
    }

    /// Asks everything in the prefix to exit, then waits for it to.
    ///
    /// # Errors
    ///
    /// Never actually fails today — kept as a `Result` so a future
    /// signal-delivery failure can surface without an API break.
    #[inline]
    pub async fn stop(&self) -> anyhow::Result<()> {
        os::stop_prefix(self).await;
        Ok(())
    }

    /// Resolves once the prefix has no live processes left.
    ///
    /// A game that re-execs itself and detaches (GOG's own launchers do)
    /// outlives the `umu-run` that started it, so waiting on the launcher
    /// alone reports the session as over while the game is still up.
    #[inline]
    pub async fn wait_until_idle(&self) {
        let started = Instant::now();
        while !self.processes().is_empty() {
            sleep(IDLE_POLL_INTERVAL).await;
        }
        tracing::debug!(prefix = %self.path.display(), elapsed = ?started.elapsed(), "prefix went idle");
    }
}

/// Every prefix with a session in flight.
#[inline]
#[must_use]
pub fn live_prefixes() -> Vec<Prefix> {
    os::live_prefixes()
}

/// A one-shot process listing per prefix, for callers watching several at
/// once.
#[inline]
#[must_use]
pub fn processes_by_prefix(prefixes: &[Prefix]) -> HashMap<PathBuf, Vec<u32>> {
    os::processes_by_prefix(prefixes)
}
