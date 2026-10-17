use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

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

pub fn restrict_to_owner(options: &mut tokio::fs::OpenOptions) {
    options.mode(0o600);
}

/// # Errors
/// The I/O error from changing `path`'s mode.
pub async fn make_executable(path: &Path) -> io::Result<()> {
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await
}

/// Only returns when `command` couldn't take this process's place.
pub fn replace_process(command: &mut Command) -> io::Error {
    command.exec()
}
