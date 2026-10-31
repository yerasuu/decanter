//! The app's system calls, one file per OS. This is the only place in the
//! crate that asks which OS it was built for; everything else calls what
//! the OS's file exports, under the same names on every OS.

#[cfg(not(target_os = "linux"))]
mod close_hotkey;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "linux"))]
mod system_tray;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

pub use os::{
    CLOSE_GAME_HINT, CLOSE_GAME_LABEL, CloseWatch, NATIVE_FRAME_CONTROLS, check_writable, create_private_dir,
    exit_signal, hostname, is_kde_wayland, is_owner_only, keep_platform, kwin_outputs, own_process_group,
    place_windows_on, refresh_tray, replace_process, restrict_to_owner, set_app_identity, spawn_tray,
    status_notifier_host, stop_placing_windows, with_window_icon, without_console,
};

/// An output as KDE lists it, for the one session where decanter asks KWin
/// to place its windows.
pub struct KwinOutput {
    pub name: ConnectorName,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

/// A connector name (`DP-3`, `HDMI-A-1`) that is safe to write into the
/// KWin script as a string literal: 1 to 64 ASCII letters, digits, `_`,
/// `.` or `-`.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectorName(String);

impl ConnectorName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ConnectorName {
    /// The rejected name, for the log.
    type Error = String;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        let is_valid = (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if is_valid { Ok(Self(name)) } else { Err(name) }
    }
}
