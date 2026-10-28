//! What getting a port onto each system and running its jobs takes, one module per OS: the only place in
//! the crate that asks which OS it was built for.
//!
//! The ports' modules compile on every system so that one system's `cargo check` also covers the others'
//! orchestration; which one runs is decided at run time ([`decanter_core::platform::support`]). Only the
//! jobs' system calls are picked by `cfg`.

pub mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) mod unported;
pub mod windows;

#[cfg(target_os = "linux")]
use linux::sys;
#[cfg(target_os = "macos")]
use macos as sys;
#[cfg(windows)]
use windows::sys;

#[cfg(target_os = "linux")]
pub(crate) use linux::runner as ports;
#[cfg(target_os = "macos")]
pub(crate) use unported as ports;
#[cfg(windows)]
pub(crate) use windows::runner as ports;

pub(crate) use sys::{
    Confined, DIRECT_LAUNCH, GIT_TLS_OPTIONS, NULL_DEVICE, build_env, confine, own_process_group, signal_of,
    terminate_group, without_console, zip_tar,
};
