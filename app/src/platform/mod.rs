//! The app's system calls, one file per OS. This is the only place in the
//! crate that asks which OS it was built for; everything else calls what
//! the OS's file exports, under the same names on every OS.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as os;
#[cfg(target_os = "macos")]
use macos as os;
#[cfg(windows)]
use windows as os;

pub use os::{
    CloseWatch, NATIVE_FRAME_CONTROLS, check_writable, exit_signal, own_process_group, replace_process,
    restrict_to_owner, spawn_tray, status_notifier_host, with_window_icon,
};
