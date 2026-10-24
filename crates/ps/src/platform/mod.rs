//! The jobs' system calls, one file per OS: the only place in the crate
//! that asks which OS it was built for.

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
    Confined, DIRECT_LAUNCH, build_env, confine, own_process_group, signal_of, terminate_group, without_console,
    zip_tar,
};
