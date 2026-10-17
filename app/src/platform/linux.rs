mod notice;
mod pads;
mod shortcut;

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Command;

use nix::unistd::{AccessFlags, access};

/// What a running game can be asked to close from, and where the notice
/// about it shows: the session bus (notifications, the Plasma shortcut)
/// and the gamepads' evdev nodes.
pub struct CloseWatch {
    bus: Option<zbus::Connection>,
    shortcut: Option<shortcut::Shortcut>,
    pads: pads::Pads,
}

impl CloseWatch {
    pub async fn open() -> Self {
        let bus = zbus::Connection::session()
            .await
            .inspect_err(|err| tracing::warn!(error = %err, "no session bus, closing a game has no shortcut or notice"))
            .ok();
        let shortcut = match &bus {
            Some(bus) => shortcut::Shortcut::register(bus).await,
            None => None,
        };
        Self {
            bus,
            shortcut,
            pads: pads::Pads::new(),
        }
    }

    /// Where the request came from, once one comes. Cancel-safe.
    pub async fn asked(&mut self) -> &'static str {
        tokio::select! {
            () = self.pads.chord() => "gamepad",
            () = shortcut::pressed(self.shortcut.as_mut()) => "keyboard",
        }
    }

    /// Borrows only what the notice needs, so its futures stay `Send`
    /// while the pads, which aren't `Sync`, sit out.
    pub const fn notices(&self) -> Notices<'_> {
        Notices {
            bus: self.bus.as_ref(),
            has_shortcut: self.shortcut.is_some(),
        }
    }

    pub async fn release(self) {
        if let Some(shortcut) = self.shortcut {
            shortcut.release().await;
        }
    }
}

#[derive(Clone, Copy)]
pub struct Notices<'watch> {
    bus: Option<&'watch zbus::Connection>,
    has_shortcut: bool,
}

impl Notices<'_> {
    pub async fn closing(self, name: &str) -> Option<u32> {
        notice::closing(self.bus, name, self.has_shortcut).await
    }

    pub async fn cancelled(self, replaces: Option<u32>, name: &str) {
        notice::cancelled(self.bus, replaces, name).await;
    }

    pub async fn close(self, id: Option<u32>) {
        notice::close(self.bus, id).await;
    }
}

/// # Errors
/// Why `dir` can't be written to by this process.
pub fn check_writable(dir: &Path) -> io::Result<()> {
    access(dir, AccessFlags::W_OK).map_err(io::Error::from)
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
