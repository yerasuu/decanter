use std::io;
use std::path::Path;
use std::process::Command;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::tray::TrayCommand;

/// Closing a game from a pad or a global shortcut, and the notice about
/// it, are a Linux desktop session's (evdev, D-Bus): here nothing asks.
pub struct CloseWatch;

#[expect(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    clippy::unused_self,
    clippy::needless_pass_by_ref_mut,
    reason = "the same signatures as the Linux watch, which awaits the bus and the pads"
)]
impl CloseWatch {
    pub async fn open() -> Self {
        tracing::info!("no gamepad chord, shortcut or notice to close a game with on this system");
        Self
    }

    pub async fn asked(&mut self) -> &'static str {
        std::future::pending().await
    }

    pub const fn notices(&self) -> Notices {
        Notices
    }

    pub async fn release(self) {}
}

#[derive(Clone, Copy)]
pub struct Notices;

#[expect(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    reason = "the same signatures as the Linux notices, which await the bus"
)]
impl Notices {
    pub async fn closing(self, _name: &str) -> Option<u32> {
        None
    }

    pub async fn cancelled(self, _replaces: Option<u32>, _name: &str) {}

    pub async fn close(self, _id: Option<u32>) {}
}

/// No tray here yet: the capability table leaves it off, so nothing asks;
/// the channel is closed from the start.
pub fn spawn_tray() -> UnboundedReceiver<TrayCommand> {
    tracing::warn!("asked for a tray, which this system doesn't have");
    unbounded_channel().1
}

/// No tray here to redraw.
pub const fn refresh_tray() {}

/// Nothing answers for a tray here yet; see the capability table.
pub async fn status_notifier_host() -> bool {
    false
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
