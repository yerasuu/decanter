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

#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use std::{io, thread};

#[cfg(not(windows))]
use std::fs;

/// What starts the client when `steam` isn't on `PATH`.
const LAUNCHER: &str = if cfg!(windows) { "steam.exe" } else { "steam.sh" };

/// How long [`wait_for_exit`] waits for Steam's main process to disappear
/// from `/proc` after asking it to shut down, before giving up. Measured
/// against a real Steam Deck client (with its usual pile of
/// `steamwebhelper`/CEF zygote children): a clean `-shutdown` took longer
/// than a first attempt at 20s allowed for, so this is generous on
/// purpose rather than trimmed to the common case.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(60);

/// What can be said about the real Steam client from here. `Unknown` is
/// its own answer rather than folded into either side, because writing
/// `shortcuts.vdf` under a live Steam loses the change when Steam exits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SteamState {
    Running,
    NotRunning,
    Unknown,
}

/// Whether Steam's own client process is running, asked first of `/proc`
/// and then of Steam's single-instance FIFO (`.steam/steam.pipe`), which
/// its main process holds open for reading while it is up.
///
/// The FIFO is what makes this work from a container without host PIDs
/// (Flatpak, distrobox with its own pid namespace): `/proc` shows none of
/// the host's processes there, but whether a FIFO has a reader is kernel
/// state on the shared home, the same from every pid namespace. Neither
/// `.steam/steam.pid` (left behind on exit, and naming a pid the
/// container can't see) nor `registry.vdf`'s `SteamPID` (left set after a
/// crash) can answer that.
#[cfg(not(windows))]
#[must_use]
#[inline]
pub fn state(steam_root: &Path) -> SteamState {
    if has_steam_process(steam_root) {
        return SteamState::Running;
    }
    probe_pipe(steam_root)
}

/// Whether a `steam.exe` is running, asked of `tasklist`, which lists the
/// image names of every process in any language: the client's own, not its
/// `steamwebhelper.exe` children.
#[cfg(windows)]
#[must_use]
#[inline]
pub fn state(_steam_root: &Path) -> SteamState {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let listing = Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq steam.exe", "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match listing {
        Ok(output) if output.status.success() => {
            if String::from_utf8_lossy(&output.stdout)
                .to_ascii_lowercase()
                .contains("\"steam.exe\"")
            {
                SteamState::Running
            } else {
                SteamState::NotRunning
            }
        }
        Ok(output) => {
            tracing::warn!(status = %output.status, "tasklist failed, cannot tell whether steam is running");
            SteamState::Unknown
        }
        Err(err) => {
            tracing::warn!(%err, "could not run tasklist, cannot tell whether steam is running");
            SteamState::Unknown
        }
    }
}

/// Scans `/proc/*/exe` for a binary named exactly `steam` living under
/// `steam_root` — Steam's own client, not one of its many
/// `steamwebhelper`/zygote children.
#[cfg(not(windows))]
fn has_steam_process(steam_root: &Path) -> bool {
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

/// Opens the write end of `steam.pipe` without blocking and without
/// writing to it: the kernel refuses with `ENXIO` exactly when nothing
/// holds the read end, and Steam's own launcher opens and closes writers
/// on it all the time, so probing is invisible to the running client.
///
/// The pipe sits in the `.steam` folder of the home Steam runs with,
/// which for every root `steam_root_candidates` knows (including the
/// Flatpak one under `~/.var/app`) is the nearest ancestor holding one.
#[cfg(target_os = "linux")]
fn probe_pipe(steam_root: &Path) -> SteamState {
    use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};

    use nix::errno::Errno;
    use nix::fcntl::OFlag;

    let Some((pipe, meta)) = steam_root.ancestors().find_map(|dir| {
        let pipe = dir.join(".steam").join("steam.pipe");
        pipe.symlink_metadata().ok().map(|meta| (pipe, meta))
    }) else {
        tracing::warn!(steam_root = %steam_root.display(), "no steam.pipe found, cannot tell whether steam is running");
        return SteamState::Unknown;
    };
    // Anything but a FIFO would accept the open regardless of Steam, so
    // the probe would say nothing.
    if !meta.file_type().is_fifo() {
        tracing::warn!(pipe = %pipe.display(), "steam.pipe is not a fifo, cannot tell whether steam is running");
        return SteamState::Unknown;
    }
    match fs::OpenOptions::new()
        .write(true)
        .custom_flags(OFlag::O_NONBLOCK.bits())
        .open(&pipe)
    {
        Ok(_reader_present) => SteamState::Running,
        Err(err) if err.raw_os_error().map(Errno::from_raw) == Some(Errno::ENXIO) => SteamState::NotRunning,
        Err(err) => {
            tracing::warn!(pipe = %pipe.display(), %err, "failed to probe steam.pipe, cannot tell whether steam is running");
            SteamState::Unknown
        }
    }
}

/// Outside Linux `/proc` is all this ever looked at; nothing changes there.
#[cfg(not(any(target_os = "linux", windows)))]
const fn probe_pipe(_steam_root: &Path) -> SteamState {
    SteamState::NotRunning
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
/// `steam_root`'s own [`LAUNCHER`] when `steam` isn't on `PATH` —
/// the same binary Steam Deck's own desktop entry resolves to, but not
/// guaranteed to be there in every environment this runs in.
fn run_steam(steam_root: &Path, arg: &str) -> io::Result<()> {
    let mut command = Command::new("steam");
    crate::strip_launcher_env(&mut command);
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

    let mut fallback = Command::new(steam_root.join(LAUNCHER));
    crate::strip_launcher_env(&mut fallback);
    if !arg.is_empty() {
        fallback.arg(arg);
    }
    fallback.spawn().map(|_child| ())
}

/// Blocks until [`state`] reports Steam gone, or [`SHUTDOWN_TIMEOUT`]
/// passes.
///
/// # Errors
/// Returns [`io::ErrorKind::TimedOut`] if Steam is still running (or can
/// no longer be told apart from running) once the timeout elapses — a
/// caller should not proceed to overwrite `shortcuts.vdf` in that case,
/// since the still-running instance would just clobber it again.
#[inline]
pub fn wait_for_exit(steam_root: &Path) -> io::Result<()> {
    let start = Instant::now();
    while state(steam_root) != SteamState::NotRunning {
        if start.elapsed() > SHUTDOWN_TIMEOUT {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "steam did not exit in time"));
        }
        thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}
