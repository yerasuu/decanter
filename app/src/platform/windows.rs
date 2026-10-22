use std::io;
use std::path::Path;
use std::process::Command;

pub use super::close_hotkey::{CLOSE_GAME_HINT, CLOSE_GAME_LABEL, CloseWatch};
pub use super::system_tray::{refresh_tray, spawn_tray};

/// The taskbar's notification area is always there.
pub async fn status_notifier_host() -> bool {
    true
}

/// Only the read-only attribute is checked: the folder's ACL decides the
/// rest, and a write that it refuses fails with its own error.
///
/// # Errors
/// Why `dir` can't be read, or `PermissionDenied` when it's read-only.
pub fn check_writable(dir: &Path) -> io::Result<()> {
    if std::fs::metadata(dir)?.permissions().readonly() {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(())
}

/// The computer's NetBIOS name, which Windows exports to every process.
pub fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .inspect_err(|err| tracing::warn!(%err, "could not read the host name"))
        .ok()
}

pub const fn restrict_to_owner(_options: &mut tokio::fs::OpenOptions) {}

/// Process groups are a Unix job-control idea: an opened app shares
/// nothing with decanter's console here to begin with.
pub const fn own_process_group(_command: &mut Command) {}

pub fn replace_process(_command: &mut Command) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "replacing this process only exists on Unix")
}

/// Only Ctrl+C has a signal here; logging off closes the window, which
/// quits the ordinary way.
pub async fn exit_signal() -> bool {
    match tokio::signal::ctrl_c().await {
        Ok(()) => {
            tracing::info!("received ctrl-c, stopping games before exit");
            true
        }
        Err(err) => {
            tracing::warn!(signal = "ctrl-c", %err, "cannot watch for the signal");
            false
        }
    }
}

/// So the task bar groups and pins decanter's windows under a name of its
/// own rather than one Windows derives from the executable's path, which
/// changes when decanter.exe moves. Also what GPUI's notifications need
/// outside an MSIX package.
pub fn set_app_identity(cx: &gpui::App) {
    cx.set_app_identity("decanter", "Decanter");
}

/// The window takes its icon from the executable here.
pub const fn with_window_icon(options: gpui::WindowOptions) -> gpui::WindowOptions {
    options
}

/// Windows drops its caption only for a transparent title bar, and then
/// hit-tests the areas marked with `window_control_area` itself: moving,
/// snapping, double-click maximize and the buttons' snap layouts come from
/// the OS, the frame keeps its own resize borders, and GPUI's
/// `start_window_move`, `start_window_resize` and `zoom_window` (which only
/// maximizes) do nothing useful here.
pub const NATIVE_FRAME_CONTROLS: bool = true;
