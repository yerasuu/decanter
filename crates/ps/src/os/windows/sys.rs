//! Windows: no process groups or signals; a game runs in the app's Job Object session instead, and a
//! build's tools in a Job Object of their own ([`confine`]).

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use tokio::process::{Child, Command};
use win32job::{ExtendedLimitInfo, Job, PriorityClass};

use crate::process::JobKind;

/// `CREATE_NO_WINDOW`, so `tar.exe` doesn't flash a console over decanter.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `psport.exe` from the Windows package or build in use ([`crate::os::windows::package`],
/// [`crate::os::windows::winbuild`]) or a folder the user picked, started directly ([`crate::bbport::direct`]): no
/// checkout, toolchain or `run.sh` here.
pub const DIRECT_LAUNCH: bool = true;

/// Where nothing is read from, such as git's hooks.
pub const NULL_DEVICE: &str = "NUL";

/// Git for Windows' `MinGit` names its CA bundle in its system configuration, which `GIT_CONFIG_NOSYSTEM` leaves out,
/// so certificates are checked against Windows' own store instead
/// ([http.sslBackend](https://git-scm.com/docs/git-config#Documentation/git-config.txt-httpsslBackend)).
pub const GIT_TLS_OPTIONS: &[&str] = &["-c", "http.sslBackend=schannel"];

/// Process groups are a Unix job-control idea; a game's Job Object is its
/// session's, made where the app starts it.
pub const fn own_process_group(_command: &mut Command) {}

/// A game here is stopped through its session, and no other job runs here
/// (a Windows build is stopped through its own Job Object); a job reaching
/// this is a bug worth seeing, not a silent no-op.
pub fn terminate_group(pid: u32, kind: JobKind) {
    tracing::error!(
        pid,
        job = kind.name(),
        "cannot stop a job's process group on this system"
    );
}

/// As [`terminate_group`]: nothing here runs in a process group.
pub fn kill_group(pid: u32, kind: JobKind) {
    tracing::error!(
        pid,
        job = kind.name(),
        "cannot kill a job's process group on this system"
    );
}

/// No signals end a process here, only exit codes.
pub const fn signal_of(_status: ExitStatus) -> Option<(i32, bool)> {
    None
}

/// Past this working set Windows trims a build tool's pages rather than lets it grow: a cap on a
/// runaway compiler, well above what the largest translation units and the GPU library's link take.
const BUILD_WORKING_SET: usize = 6 << 30;

/// Windows' own `tar.exe` (libarchive's bsdtar, which reads zip archives), by its full path in the
/// system folder so that no other `tar` on `PATH`, such as Git's GNU tar, is taken for it.
pub fn zip_tar() -> PathBuf {
    system32().join("tar.exe")
}

/// `%SystemRoot%`, or where Windows installs itself when it is unset.
fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

/// Where Windows' own programs are.
fn system32() -> PathBuf {
    system_root().join("System32")
}

/// So a console program started for decanter's own use, such as `tar.exe`, opens no window.
pub fn without_console(command: &mut Command) {
    command.creation_flags(CREATE_NO_WINDOW);
}

/// A Windows build's tools: the Job Object holding the first one and everything it starts.
pub struct Confined(Job);

/// `child` in a Job Object of its own that ends every process in it when its last handle closes: at
/// [`Confined::kill`], once the tool exits, or when decanter itself ends. Without
/// `JOB_OBJECT_LIMIT_BREAKAWAY_OK` or `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK` no child can leave it
/// ([Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)). A lower
/// priority and [`BUILD_WORKING_SET`] are asked for too, but Windows may refuse them to a standard
/// user, which the log says.
///
/// # Errors
/// When `child` has no handle, or the job can't be made or take it: the caller must not let the
/// tool run unconfined.
// NOTE: as with a game's session, `child` runs for a moment before it joins, and a process it starts
// in that window escapes the job; closing it needs the child created suspended, which std's
// `Command` has no safe way to do.
pub fn confine(child: &Child) -> io::Result<Confined> {
    let Some(handle) = child
        .raw_handle()
        .and_then(|handle| isize::try_from(handle.addr()).ok())
    else {
        return Err(io::Error::other("the tool has no process handle"));
    };
    let mut info = ExtendedLimitInfo::new();
    info.limit_kill_on_job_close();
    let job = match Job::create_with_limit_info(&info) {
        Ok(job) => job,
        Err(err) => return Err(io::Error::other(format!("cannot create a job object: {err:?}"))),
    };
    let mut lower = ExtendedLimitInfo::new();
    lower
        .limit_kill_on_job_close()
        .limit_priority_class(PriorityClass::BelowNormal)
        .limit_working_memory(1 << 20, BUILD_WORKING_SET);
    if let Err(err) = job.set_extended_limit_info(&lower) {
        tracing::warn!(error = ?err, "the build's job keeps the normal priority and no working-set cap");
    }
    match job.assign_process(handle) {
        Ok(()) => Ok(Confined(job)),
        Err(err) => Err(io::Error::other(format!(
            "cannot put the tool in its job object: {err:?}"
        ))),
    }
}

impl Confined {
    /// How many processes the job holds; a count past what the job can list reads as too many.
    pub fn processes(&self) -> usize {
        self.0.query_process_id_list().map_or_else(
            |err| {
                tracing::warn!(error = ?err, "cannot list the build's processes");
                usize::MAX
            },
            |pids| pids.len(),
        )
    }

    /// Closes the job, which ends every process in it.
    pub fn kill(self) {
        drop(self.0);
    }
}

/// What a build's tools see: no variable of the user's, `PATH` only the system folder (Ninja runs
/// CMake's custom commands through `cmd.exe /C`), the system folders Windows' own DLLs look for,
/// and temporary files in `temp`.
pub fn build_env(temp: &Path) -> Vec<(&'static str, OsString)> {
    let root = system_root();
    let system32 = system32();
    let mut env = vec![
        ("SystemRoot", root.clone().into_os_string()),
        ("windir", root.into_os_string()),
        ("ComSpec", system32.join("cmd.exe").into_os_string()),
        ("PATH", system32.into_os_string()),
        ("PATHEXT", ".COM;.EXE;.BAT;.CMD".into()),
        ("TEMP", temp.as_os_str().to_owned()),
        ("TMP", temp.as_os_str().to_owned()),
    ];
    if let Some(drive) = std::env::var_os("SystemDrive") {
        env.push(("SystemDrive", drive));
    }
    env
}
