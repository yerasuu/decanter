//! Linux: process groups and signals.

use std::ffi::OsString;
use std::io;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::process::{Child, Command};

use crate::process::JobKind;

/// The port runs from a checkout, a fetched build or a bundle, through `run.sh`.
pub const DIRECT_LAUNCH: bool = false;

/// Where nothing is read from, such as git's hooks.
pub const NULL_DEVICE: &str = "/dev/null";

/// git's own certificate store serves here.
pub const GIT_TLS_OPTIONS: &[&str] = &[];

/// So a stop reaches everything the job started: `run.sh` and the game,
/// nix-shell and what runs inside it, or a build's tools.
pub fn own_process_group(command: &mut Command) {
    command.process_group(0);
}

/// SIGTERM to the whole group, so the job can wrap up on its own.
pub fn terminate_group(pid: u32, kind: JobKind) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    if let Err(err) = killpg(Pid::from_raw(pid), Signal::SIGTERM) {
        tracing::warn!(pid, job = kind.name(), %err, "could not stop job");
    }
}

/// SIGKILL to the whole group, for a job still running past [`terminate_group`]'s grace.
pub fn kill_group(pid: u32, kind: JobKind) {
    let Ok(pid) = i32::try_from(pid) else {
        return;
    };
    if let Err(err) = killpg(Pid::from_raw(pid), Signal::SIGKILL) {
        tracing::warn!(pid, job = kind.name(), %err, "could not kill job");
    }
}

/// The signal that ended the process, and whether it dumped core.
pub fn signal_of(status: ExitStatus) -> Option<(i32, bool)> {
    status.signal().map(|signal| (signal, status.core_dumped()))
}

/// libarchive's `bsdtar`, which reads zip archives where GNU `tar` doesn't: psport's Windows package
/// is only installed on Windows, but its checks run the same here.
pub fn zip_tar() -> PathBuf {
    PathBuf::from("bsdtar")
}

/// Only Windows opens a console for a console program started from a window.
pub const fn without_console(_command: &mut Command) {}

/// A Windows build's tools where the same orchestration runs to be checked: their process group,
/// which [`own_process_group`] gave them before they started.
pub struct Confined(i32);

/// # Errors
/// When `child` has already exited, so its group can't be told.
pub fn confine(child: &Child) -> io::Result<Confined> {
    child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .map(Confined)
        .ok_or_else(|| io::Error::other("the tool exited before it could be followed"))
}

impl Confined {
    /// Not counted: nothing lists a process group's members without walking `/proc`, so no cap is
    /// ever passed here.
    pub const fn processes(&self) -> usize {
        0
    }

    /// SIGKILL to the whole group; one already gone is the outcome asked for.
    pub fn kill(self) {
        if let Err(err) = killpg(Pid::from_raw(self.0), Signal::SIGKILL) {
            tracing::debug!(pgid = self.0, %err, "the build's process group was already gone");
        }
    }
}

/// What a build's tools see here: the system's programs, and its temporary files in `temp`.
pub fn build_env(temp: &Path) -> Vec<(&'static str, OsString)> {
    vec![
        ("PATH", "/usr/bin:/bin".into()),
        ("HOME", temp.as_os_str().to_owned()),
        ("TMPDIR", temp.as_os_str().to_owned()),
    ]
}
