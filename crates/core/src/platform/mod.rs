//! decanter's system calls, OS conventions and capability table, one file
//! per OS.
//!
//! The only place in the crate that asks which OS it was built for; the
//! other crates reach the same functions here for what they share.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

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
    FolderIdentity, config_dir, default_library_dir, disk_space, executable_permissions, folder_identity,
    legacy_data_dir, lock_dir, others_running, steam_root_candidates, support, symlink_dir, symlink_file,
};
