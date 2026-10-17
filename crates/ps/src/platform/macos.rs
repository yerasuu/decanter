//! macOS: process groups and signals.

use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::process::Command;

use crate::process::JobKind;

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

/// The signal that ended the process, and whether it dumped core.
pub fn signal_of(status: ExitStatus) -> Option<(i32, bool)> {
    status.signal().map(|signal| (signal, status.core_dumped()))
}
