//! A Proton prefix decanter owns, and tracking which ones currently have
//! something running in them.

use core::str::from_utf8;
use core::time::Duration;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use anyhow::Context as _;
use futures_util::future::join_all;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::time::sleep;

use crate::launch::{Launch, start};
use crate::proton::Proton;

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
        let mut launch = Launch::new("wineboot", "umu-decanter");
        launch.args.push("-i".to_owned());
        launch.proton = proton;
        launch.working_directory = Some(env::temp_dir());
        let session = match start(self, launch).await {
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
    /// launcher's own children included.
    ///
    /// Read off `/proc` rather than asked of the prefix: Wine propagates
    /// `WINEPREFIX` to everything it starts, pressure-vessel shares the
    /// host pid namespace, and this needs no Proton path, no in-prefix
    /// agent, and nothing running to answer.
    #[inline]
    #[must_use]
    pub fn processes(&self) -> Vec<u32> {
        let Ok(entries) = fs::read_dir("/proc") else {
            tracing::warn!("/proc unreadable, reporting no live processes for any prefix");
            return Vec::new();
        };
        let pids: Vec<u32> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let pid_str = match name.to_str() {
                    Some(pid_str) => pid_str,
                    None => return None,
                };
                let pid: u32 = match pid_str.parse().ok() {
                    Some(pid) => pid,
                    None => return None,
                };
                // Unreadable environ means someone else's process (or one
                // that exited mid-scan) — either way, not ours. A `let-else`
                // over a `match` with a bare `Err(_)` arm, which this
                // codebase's own humanization scanner rejects. Not logged:
                // this is the expected outcome for most of `/proc` on every
                // scan, so a per-pid line here would be noise, not signal.
                let Some(environ) = fs::read(entry.path().join("environ")).ok() else {
                    return None;
                };
                environ_has_prefix(&environ, &self.path).then_some(pid)
            })
            .collect();
        tracing::trace!(prefix = %self.path.display(), found = pids.len(), "scanned /proc for this prefix's processes");
        pids
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
    /// `SIGTERM`, not `SIGKILL`: Wine's own processes flush and shut the
    /// prefix down cleanly on it, and a game killed outright is a game
    /// that loses whatever it hadn't written yet.
    ///
    /// # Errors
    ///
    /// Never actually fails today — kept as a `Result` so a future
    /// signal-delivery failure can surface without an API break.
    #[inline]
    pub async fn stop(&self) -> anyhow::Result<()> {
        tracing::debug!(prefix = %self.path.display(), "stopping prefix");
        // Dropped from the live set first: a stop that is already under
        // way shouldn't be started a second time by a shutdown racing it.
        live_lock().remove(&self.path);
        // Re-signals every tick rather than sweeping once: a process that
        // appears after the first sweep (a game re-execing itself during
        // shutdown, which GOG's own launchers do) would otherwise never
        // receive SIGTERM, and the loop below would wait on it forever.
        loop {
            let pids = self.processes();
            if pids.is_empty() {
                break;
            }
            for pid in pids {
                // A pid outside `Pid`'s range can't belong to a real
                // process; skip rather than error, same as an already-gone one.
                let Ok(signed_pid) = i32::try_from(pid) else {
                    tracing::warn!(pid, "pid out of range for a signal, skipping");
                    continue;
                };
                // Racy by nature — the pid may already be gone, which is
                // the outcome being asked for anyway, so a failed send is
                // traced rather than warned about.
                let result = kill(Pid::from_raw(signed_pid), Signal::SIGTERM);
                tracing::trace!(pid, ?result, "sent SIGTERM");
            }
            sleep(IDLE_POLL_INTERVAL).await;
        }
        tracing::debug!(prefix = %self.path.display(), "prefix has no live processes left");
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

/// The prefixes decanter has started something in and not seen finish.
///
/// Kept apart from whatever the UI is showing, because this is what
/// shutdown reads: a game outlives the launcher that started it — umu
/// execs into a container and Proton detaches — so nothing stops on its
/// own when decanter goes away. Closing the app has to go and stop them.
fn live() -> &'static Mutex<HashSet<PathBuf>> {
    static LIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// Locks [`live`], recovering a poisoned lock rather than discarding its
/// contents: a panic while holding it must not make `stop_all()` believe no
/// games are running and leave them alive.
pub(crate) fn live_lock() -> MutexGuard<'static, HashSet<PathBuf>> {
    live().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every prefix with a session in flight.
#[inline]
#[must_use]
pub fn live_prefixes() -> Vec<Prefix> {
    live_lock().iter().cloned().map(Prefix::new).collect()
}

/// Stops everything decanter started, at once.
///
/// For shutdown. Concurrent rather than one after another: each stop waits
/// out its own prefix, and closing the app shouldn't take as long as the
/// sum of them.
#[inline]
pub async fn stop_all() {
    let prefixes = live_prefixes();
    if prefixes.is_empty() {
        return;
    }
    tracing::info!("stopping {} running game(s)", prefixes.len());
    let stopping = prefixes.iter().map(Prefix::stop);
    join_all(stopping).await;
}

/// Whether a `/proc/<pid>/environ` blob belongs to a process running
/// against `prefix`.
///
/// The blob is NUL-separated `KEY=VALUE` entries. `WINEPREFIX` covers Wine
/// and the game; `STEAM_COMPAT_DATA_PATH` covers umu and Proton
/// themselves, which are already running before Wine sets anything.
fn environ_has_prefix(environ: &[u8], prefix: &Path) -> bool {
    let Some(prefix_str) = prefix.to_str() else {
        return false;
    };
    environ.split(|byte| *byte == 0).any(|bytes| {
        let Ok(text) = from_utf8(bytes) else {
            return false;
        };
        let Some((key, value)) = text.split_once('=') else {
            return false;
        };
        matches!(key, "WINEPREFIX" | "STEAM_COMPAT_DATA_PATH") && value == prefix_str
    })
}

/// Reduces a caller-supplied relative path to something that can only ever
/// land inside the directory it is joined to: no absolute paths, no `..`,
/// no drive prefixes.
///
/// Every path decanter joins onto a prefix or a data directory comes from
/// a storefront's manifest or a product id, i.e. from the network.
#[inline]
#[must_use]
pub fn sanitize_relative_path(path: &str) -> Option<PathBuf> {
    let source = Path::new(path);
    let mut sanitized = PathBuf::new();
    for component in source.components() {
        match component {
            Component::Normal(part) => sanitized.push(part),
            // `.` is meaningless rather than dangerous; the rest are how a
            // relative path escapes upward.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                tracing::warn!(path, "rejecting path: climbs out of or escapes its base directory");
                return None;
            }
        }
    }
    if sanitized.as_os_str().is_empty() {
        tracing::warn!(path, "rejecting path: sanitizes to nothing");
        return None;
    }
    Some(sanitized)
}

/// A one-shot process listing per prefix, for callers watching several at
/// once — `/proc` gets scanned once instead of once per prefix.
#[inline]
#[must_use]
pub fn processes_by_prefix(prefixes: &[Prefix]) -> HashMap<PathBuf, Vec<u32>> {
    let mut found: HashMap<PathBuf, Vec<u32>> = prefixes
        .iter()
        .map(|prefix| (prefix.path.clone(), Vec::new()))
        .collect();
    let Ok(entries) = fs::read_dir("/proc") else {
        tracing::warn!("/proc unreadable, reporting no live processes for any prefix");
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        // Same as `Prefix::processes`: unreadable for most of `/proc` on
        // every scan, so not logged per-pid.
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        for prefix in prefixes {
            if environ_has_prefix(&environ, &prefix.path) {
                found.entry(prefix.path.clone()).or_default().push(pid);
            }
        }
    }
    tracing::trace!(
        prefixes = prefixes.len(),
        matched = found.values().map(Vec::len).sum::<usize>(),
        "scanned /proc for several prefixes at once"
    );
    found
}
