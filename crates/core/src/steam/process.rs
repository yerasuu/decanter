//! Starting, stopping, and detecting the real Steam client process —
//! needed because Steam holds its own in-memory copy of `shortcuts.vdf`
//! while it runs and periodically re-saves it, clobbering an on-disk edit
//! made while it's still up. See `super::export`'s pending-file staging,
//! which this exists to unblock: apply a staged change, Steam has to be
//! down first.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use std::{fs, io, thread};

/// How long [`wait_for_exit`] waits for Steam's main process to disappear
/// from `/proc` after asking it to shut down, before giving up. Measured
/// against a real Steam Deck client (with its usual pile of
/// `steamwebhelper`/CEF zygote children): a clean `-shutdown` took longer
/// than a first attempt at 20s allowed for, so this is generous on
/// purpose rather than trimmed to the common case.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60);

/// Whether Steam's own client process (not one of its many `steamwebhelper`/
/// zygote children) is currently running, by scanning `/proc/*/exe` for a
/// binary named exactly `steam` living under `steam_root`.
///
/// Linux-only, like every other consumer of this: `/proc` is how a
/// process's own executable is looked up here, since there is no `steam.pid`
/// or `registry.vdf` "running" flag to read on every Steam build instead.
#[must_use]
#[inline]
pub fn is_steam_running(steam_root: &Path) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let is_pid = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.chars().all(|ch| ch.is_ascii_digit()));
        if !is_pid {
            continue;
        }
        let Ok(exe) = fs::read_link(entry.path().join("exe")) else {
            continue;
        };
        if exe.file_name().is_some_and(|name| name == "steam") && exe.starts_with(steam_root) {
            return true;
        }
    }
    false
}

/// Asks a running Steam client to shut down cleanly — `steam -shutdown`
/// messages the already-running instance rather than starting a second
/// one, and is Steam's own documented way to do this instead of sending
/// it a signal directly.
///
/// # Errors
/// Fails if neither `steam` on `PATH` nor `steam_root`'s own launcher
/// script could be started at all.
#[inline]
pub fn shutdown(steam_root: &Path) -> io::Result<()> {
    run_steam(steam_root, "-shutdown")
}

/// Starts Steam fresh — fire-and-forget, since Steam daemonizes itself and
/// this isn't the caller's process to wait on.
///
/// # Errors
/// Fails if neither `steam` on `PATH` nor `steam_root`'s own launcher
/// script could be started at all.
#[inline]
pub fn launch(steam_root: &Path) -> io::Result<()> {
    run_steam(steam_root, "")
}

/// Runs `steam <arg>` (skipping the arg if empty), falling back to
/// `steam_root`'s own `steam.sh` launcher when `steam` isn't on `PATH` —
/// the same binary Steam Deck's own desktop entry resolves to, but not
/// guaranteed to be there in every environment this runs in.
fn run_steam(steam_root: &Path, arg: &str) -> io::Result<()> {
    let mut command = Command::new("steam");
    sanitize_env(&mut command);
    if !arg.is_empty() {
        command.arg(arg);
    }
    // `-shutdown` is meant to signal-and-return; a plain launch is
    // fire-and-forget. Either way this process doesn't own Steam's
    // lifetime, so the spawned `Child` is dropped unused — only whether
    // the command could even start matters.
    if command.spawn().is_ok() {
        return Ok(());
    }

    let mut fallback = Command::new(steam_root.join("steam.sh"));
    sanitize_env(&mut fallback);
    if !arg.is_empty() {
        fallback.arg(arg);
    }
    fallback.spawn().map(|_child| ())
}

/// Strips library-injection variables this process might itself be
/// running under (a dev shell, some other launcher) before starting
/// Steam — confirmed the hard way: a `steam` spawned while this process
/// had a `LD_LIBRARY_PATH` full of unrelated 64-bit libraries came up
/// unable to find its own bundled 32-bit runtime and warned about missing
/// libraries. Steam's own `steam.sh` sets up whatever linking it needs
/// internally; inheriting this process's is never required and, as
/// observed, can actively break it. `GLIBC_TUNABLES` goes too: the
/// AppImage caps decanter's own malloc arenas with it, a limit meant for
/// this process, not for Steam.
fn sanitize_env(command: &mut Command) {
    command
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
        .env_remove("GLIBC_TUNABLES");
}

/// Blocks until [`is_steam_running`] reports Steam gone, or
/// [`SHUTDOWN_TIMEOUT`] passes.
///
/// # Errors
/// Returns [`io::ErrorKind::TimedOut`] if Steam is still running once the
/// timeout elapses — a caller should not proceed to overwrite
/// `shortcuts.vdf` in that case, since the still-running instance would
/// just clobber it again.
#[inline]
pub fn wait_for_exit(steam_root: &Path) -> io::Result<()> {
    let start = Instant::now();
    while is_steam_running(steam_root) {
        if start.elapsed() > SHUTDOWN_TIMEOUT {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "steam did not exit in time"));
        }
        thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}
