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
    LAUNCHER, Tracked, end, ge_proton_prepare, ge_proton_remove, ge_proton_status, ge_proton_update, live_prefixes,
    prefix_processes, prepare, processes_by_prefix, proton_status, raven_confirm, raven_head, raven_in_use,
    raven_launch, raven_log_path, raven_prepare, raven_prune, raven_remove, raven_status, raven_update, raven_use,
    spawn_failed, spawned, steam_compat_dir, stop, stop_prefix, umu_on_path, umu_prepare, umu_remove, umu_status,
    umu_update, wait_status,
};
