//! What runs in a Proton prefix: finding its processes, stopping them, and
//! which prefixes currently have a session in them.

use core::str::from_utf8;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tokio::time::sleep;

use crate::Prefix;
use crate::prefix::IDLE_POLL_INTERVAL;
use crate::session::live_lock;

/// Set on a Linux program run natively ([`crate::Launch::native`]) to its
/// prefix's path, which is then only the session's key: nothing creates
/// the folder, and the program and its children are found and stopped by
/// it the way a Proton session is found by `WINEPREFIX`.
pub(crate) const SESSION_ENV: &str = "DECANTER_SESSION";

/// Read off `/proc` rather than asked of the prefix: Wine propagates
/// `WINEPREFIX` to everything it starts, pressure-vessel shares the
/// host pid namespace, and this needs no Proton path, no in-prefix
/// agent, and nothing running to answer.
pub(crate) fn prefix_processes(prefix: &Prefix) -> Vec<u32> {
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
            environ_has_prefix(&environ, &prefix.path).then_some(pid)
        })
        .collect();
    tracing::trace!(prefix = %prefix.path.display(), found = pids.len(), "scanned /proc for this prefix's processes");
    pids
}

/// `SIGTERM`, not `SIGKILL`: Wine's own processes flush and shut the
/// prefix down cleanly on it, and a game killed outright is a game
/// that loses whatever it hadn't written yet.
pub(crate) async fn stop_prefix(prefix: &Prefix) {
    tracing::debug!(prefix = %prefix.path.display(), "stopping prefix");
    // Dropped from the live set first: a stop that is already under
    // way shouldn't be started a second time by a shutdown racing it.
    live_lock().retain(|_, live| live.path != prefix.path);
    // Re-signals every tick rather than sweeping once: a process that
    // appears after the first sweep (a game re-execing itself during
    // shutdown, which GOG's own launchers do) would otherwise never
    // receive SIGTERM, and the loop below would wait on it forever.
    loop {
        let pids = prefix_processes(prefix);
        if pids.is_empty() {
            break;
        }
        pids.into_iter().for_each(terminate);
        sleep(IDLE_POLL_INTERVAL).await;
    }
    tracing::debug!(prefix = %prefix.path.display(), "prefix has no live processes left");
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

pub(crate) fn live_prefixes() -> Vec<Prefix> {
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

/// `/proc` scanned once instead of once per prefix.
pub(crate) fn processes_by_prefix(prefixes: &[Prefix]) -> HashMap<PathBuf, Vec<u32>> {
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
