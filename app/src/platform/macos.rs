use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

pub use super::close_hotkey::{CLOSE_GAME_HINT, CLOSE_GAME_LABEL, CloseWatch, keep_platform};
pub use super::system_tray::{refresh_tray, spawn_tray};

/// The menu bar always takes status items.
pub async fn status_notifier_host() -> bool {
    true
}

/// Only the owner's mode bits are checked, without `access(2)`: decanter
/// never rewrites itself here, so nothing finer depends on it.
///
/// # Errors
/// Why `dir` can't be read, or `PermissionDenied` when it's read-only.
pub fn check_writable(dir: &Path) -> io::Result<()> {
    if std::fs::metadata(dir)?.permissions().readonly() {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(())
}

/// The Bonjour name, the one Sharing shows and that stays stable across networks.
pub fn hostname() -> Option<String> {
    Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .inspect_err(|err| tracing::warn!(%err, "could not read the host name"))
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn restrict_to_owner(options: &mut tokio::fs::OpenOptions) {
    options.mode(0o600);
}

/// decanter never installs a copy of its own here; see `self_update`.
pub async fn create_private_dir(dir: &Path) -> io::Result<()> {
    tokio::fs::create_dir_all(dir).await
}

/// No copy of decanter's own is ever run here, so nothing is trusted as one.
pub const fn is_owner_only(_path: &Path) -> bool {
    false
}

/// A console program opens no window of its own here.
pub const fn without_console(_command: &mut tokio::process::Command) {}

/// So the opened app doesn't share decanter's terminal signals.
pub fn own_process_group(command: &mut Command) {
    command.process_group(0);
}

/// Only returns when `command` couldn't take this process's place.
pub fn replace_process(command: &mut Command) -> io::Error {
    command.exec()
}

/// Resolves once SIGTERM or SIGINT came, or `false` at once when they
/// can't be watched.
pub async fn exit_signal() -> bool {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(signal) => signal,
        Err(err) => {
            tracing::warn!(signal = "SIGTERM", %err, "cannot watch for the signal");
            return false;
        }
    };
    let mut interrupt = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
        Ok(signal) => signal,
        Err(err) => {
            tracing::warn!(signal = "SIGINT", %err, "cannot watch for the signal");
            return false;
        }
    };

    tokio::select! {
        _ = term.recv() => tracing::info!("received sigterm, stopping games before exit"),
        _ = interrupt.recv() => tracing::info!("received sigint, stopping games before exit"),
    }
    true
}

/// The bundle's identifier is the identity here; GPUI has none to set.
pub const fn set_app_identity(_cx: &gpui::App) {}

/// The window takes its icon from the executable here.
pub const fn with_window_icon(options: gpui::WindowOptions) -> gpui::WindowOptions {
    options
}

/// Kept off so the system title bar stays and its traffic lights don't
/// land on top of the topbar's own buttons.
pub const NATIVE_FRAME_CONTROLS: bool = false;
