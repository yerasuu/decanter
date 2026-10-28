//! What starting, following and stopping a game takes on each system; the
//! only place that picks one by `cfg`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "linux"))]
mod native;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux as backend;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(target_os = "linux"))]
use native as backend;
#[cfg(windows)]
use windows as platform;

pub(crate) use backend::{
    LAUNCHER, Tracked, end, live_prefixes, prefix_processes, prepare, processes_by_prefix, spawn_failed, spawned,
    steam_compat_dir, stop, stop_prefix, umu_prepare, umu_remove, umu_status, umu_update, wait_status,
};
