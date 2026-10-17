//! Windows: no process groups or signals; jobs don't run here yet.

use std::process::ExitStatus;

use tokio::process::Command;

use crate::process::JobKind;

/// Process groups are a Unix job-control idea; a Job Object would be the
/// equivalent once jobs run here.
pub const fn own_process_group(_command: &mut Command) {}

/// Jobs only run on Linux today (the ports and Proton); a job reaching
/// this elsewhere is a bug worth seeing, not a silent no-op.
pub fn terminate_group(pid: u32, kind: JobKind) {
    tracing::error!(
        pid,
        job = kind.name(),
        "cannot stop a job's process group on this system"
    );
}

/// No signals end a process here, only exit codes.
pub const fn signal_of(_status: ExitStatus) -> Option<(i32, bool)> {
    None
}
