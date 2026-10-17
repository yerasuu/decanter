use std::io;
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

pub const fn restrict_to_owner(_options: &mut tokio::fs::OpenOptions) {}

/// Process groups are a Unix job-control idea: an opened app shares
/// nothing with decanter's console here to begin with.
pub const fn own_process_group(_command: &mut Command) {}

pub fn replace_process(_command: &mut Command) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "replacing this process only exists on Unix")
}
