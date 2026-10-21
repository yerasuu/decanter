use std::io;
use std::os::unix::process::CommandExt as _;
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

/// The window takes its icon from the executable here.
pub const fn with_window_icon(options: gpui::WindowOptions) -> gpui::WindowOptions {
    options
}

/// Kept off so the system title bar stays and its traffic lights don't
/// land on top of the topbar's own buttons.
pub const NATIVE_FRAME_CONTROLS: bool = false;
