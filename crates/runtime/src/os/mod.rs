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
pub use linux::{
    Prepared, Session, UMU_LAUNCHER_REPO, UMU_LAUNCHER_VERSION, UMU_PROTON_REPO, live_prefixes, prepare,
    processes_by_prefix, start, steam_compat_dir, steam_runner_exists, steam_runners, stop_all, umu_run_path,
};
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(target_os = "linux"))]
pub use native::{Prepared, Session, end, is_live, prepare, start, stop, stop_all, take_killed};
#[cfg(windows)]
use windows as platform;
