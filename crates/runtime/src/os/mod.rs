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
#[cfg(target_os = "linux")]
pub use linux::{
    UMU_LAUNCHER_REPO, UMU_LAUNCHER_VERSION, UMU_PROTON_REPO, live_prefixes, processes_by_prefix, steam_compat_dir,
    steam_runner_exists, steam_runners, umu_run_path,
};
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(target_os = "linux"))]
use native as backend;
#[cfg(windows)]
use windows as platform;

pub(crate) use backend::{LAUNCHER, Tracked, end, prepare, spawn_failed, spawned, stop, wait_status};
