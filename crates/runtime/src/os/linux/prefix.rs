//! What runs in a Proton prefix: finding its processes, stopping them, and
//! tracking which prefixes currently have something running in them.

use core::str::from_utf8;
use core::time::Duration;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context as _;
use tokio::time::sleep;

use crate::session::live_lock;
use crate::{Launch, Prefix, Proton, start};

/// How often [`Prefix::wait_until_idle`] re-checks for leftover processes.
/// Only reached after the launcher already exited, so it costs one
/// directory scan per tick and nothing while a game is actually running.
pub(crate) const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Set on a Linux program run natively ([`crate::Launch::native`]) to its
/// prefix's path, which is then only the session's key: nothing creates
/// the folder, and the program and its children are found and stopped by
/// it the way a Proton session is found by `WINEPREFIX`.
pub(crate) const SESSION_ENV: &str = "DECANTER_SESSION";

impl Prefix {
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
        let session = match start(launch).await {
            Ok(session) => session,
            Err(err) => return Err(err),
        };
        session.wait().await.context("initializing the prefix with wineboot")
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
        let Some(entries) = proc_entries() else {
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
        live_lock().retain(|_, prefix| prefix.path != self.path);
        // Re-signals every tick rather than sweeping once: a process that
        // appears after the first sweep (a game re-execing itself during
        // shutdown, which GOG's own launchers do) would otherwise never
        // receive SIGTERM, and the loop below would wait on it forever.
        loop {
            let pids = self.processes();
            if pids.is_empty() {
                break;
            }
            pids.into_iter().for_each(terminate);
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

fn terminate(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    // A pid outside `Pid`'s range can't belong to a real
    // process; skip rather than error, same as an already-gone one.
    let Ok(signed_pid) = i32::try_from(pid) else {
        tracing::warn!(pid, "pid out of range for a signal, skipping");
        return;
    };
    // Racy by nature — the pid may already be gone, which is
    // the outcome being asked for anyway, so a failed send is
    // traced rather than warned about.
    let result = kill(Pid::from_raw(signed_pid), Signal::SIGTERM);
    tracing::trace!(pid, ?result, "sent SIGTERM");
}

/// Every process's entry in `/proc`, or `None` without one to read. Off
/// Linux nothing runs in a prefix, so there is nothing to look for.
fn proc_entries() -> Option<fs::ReadDir> {
    if !crate::available() {
        return None;
    }
    fs::read_dir("/proc")
        .inspect_err(|err| tracing::warn!(%err, "/proc unreadable, reporting no live processes for any prefix"))
        .ok()
}

/// Every prefix with a session in flight.
#[inline]
#[must_use]
pub fn live_prefixes() -> Vec<Prefix> {
    let paths: HashSet<PathBuf> = live_lock().values().map(|prefix| prefix.path.clone()).collect();
    paths.into_iter().map(Prefix::new).collect()
}

/// Whether a `/proc/<pid>/environ` blob belongs to a process running
/// against `prefix`.
///
/// The blob is NUL-separated `KEY=VALUE` entries. `WINEPREFIX` covers Wine
/// and the game; `STEAM_COMPAT_DATA_PATH` covers umu and Proton
/// themselves, which are already running before Wine sets anything;
/// [`SESSION_ENV`] a native program.
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
        matches!(key, "WINEPREFIX" | "STEAM_COMPAT_DATA_PATH" | SESSION_ENV) && value == prefix_str
    })
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
    let Some(entries) = proc_entries() else {
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
