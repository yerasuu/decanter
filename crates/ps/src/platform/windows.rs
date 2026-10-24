//! Windows: no process groups or signals; a game runs in the app's Job Object session instead.

use std::path::PathBuf;
use std::process::ExitStatus;

use tokio::process::Command;

use crate::process::JobKind;

/// `CREATE_NO_WINDOW`, so `tar.exe` doesn't flash a console over decanter.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `psport.exe` from the Windows package in use ([`crate::package`]) or a folder the user picked,
/// started directly ([`crate::bbport::direct`]): no checkout, toolchain or `run.sh` here.
pub const DIRECT_LAUNCH: bool = true;

/// Process groups are a Unix job-control idea; a game's Job Object is its
/// session's, made where the app starts it.
pub const fn own_process_group(_command: &mut Command) {}

/// A game here is stopped through its session, and no other job runs here
/// (builds and conversions need the toolchain); a job reaching this is a
/// bug worth seeing, not a silent no-op.
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

/// Windows' own `tar.exe` (libarchive's bsdtar, which reads zip archives), by its full path in the
/// system folder so that no other `tar` on `PATH`, such as Git's GNU tar, is taken for it.
pub fn zip_tar() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("System32")
        .join("tar.exe")
}

/// So a console program started for decanter's own use, such as `tar.exe`, opens no window.
pub fn without_console(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}
