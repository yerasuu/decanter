//! psport's Testing channel on Windows: the branch Linux follows, built on this machine with a
//! toolchain decanter downloads and pins itself (decisions #924 and #928), since CI publishes
//! Windows builds only for tagged releases ([`crate::os::windows::package`]).
//!
//! A build fetches one commit with git ([`git`]), only after the owner confirmed that commit
//! ([`confirm`]), since building runs the branch's own CMake scripts with the user's rights. It
//! runs the pinned CMake and Ninja on it ([`toolchain`]) confined ([`run`]): no shell, a clean
//! environment, a Job Object that ends everything it started, bounded in time and processes. The
//! programs and the DLLs they load then become a folder in the shape of the published package,
//! checked as one ([`assemble`]), at `<library>/psport/psport-windows/<commit>/` beside the
//! packages, with `<commit>.json` recording it. Nothing built is run here; Jugar runs it once the
//! user chooses it.
//!
//! Nothing stops a hostile branch from reaching the network during its build: Windows offers no
//! unprivileged way short of an AppContainer, left out (decision #928). The build needs none, since
//! every dependency comes in the toolchain package, and the confirmation per commit is what stands
//! between a branch and this machine.

pub mod assemble;
pub mod git;
mod pe;
pub mod pins;
pub mod run;
pub mod toolchain;

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;

use crate::os::linux::prebuilt;
use crate::os::windows::package;
use crate::paths::Root;
use crate::remote::RemoteError;
use crate::{archive, download, os};

/// The configure step: CMake checks the compiler and reads the toolchain package.
const CONFIGURE: run::Bounds = run::Bounds {
    timeout: Duration::from_mins(15),
    max_processes: 64,
};
/// The compile: about seven minutes on psport's CI (4 vCPU, no LTO); a slow machine gets many times
/// that.
const COMPILE: run::Bounds = run::Bounds {
    timeout: Duration::from_mins(90),
    max_processes: 128,
};
/// The checkout's own path, at most: Ninja on Windows refuses a file whose full path passes 260
/// characters, and the deepest file of a psport checkout sits 190 below its root (psport's
/// `docs/en/WINDOWS_TOOLCHAIN.md`). The build folder beside it is as long, and its objects go
/// about 150 deeper.
pub const MAX_CHECKOUT_PATH: usize = 68;
/// Ninja's jobs at most, so the memory the compilers take stays within what a gaming PC has.
const MAX_JOBS: usize = 16;
const MAX_RECORD_BYTES: u64 = 64 << 10;
const MAX_CONFIRMED_BYTES: u64 = 256 << 10;
/// Confirmations kept; older ones are asked for again.
const MAX_CONFIRMED: usize = 200;
/// Any one line of output, past which the rest of the line is dropped.
pub const MAX_LINE_BYTES: usize = 4096;
/// A build's whole log file; past it, lines still reach the UI but no longer the file.
const MAX_LOG_BYTES: u64 = 64 << 20;
/// Hex digits naming a build's work folder: as many as [`MAX_CHECKOUT_PATH`] leaves room for.
const WORK_HEX: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("this decanter pins no psport toolchain package yet")]
    NotPinned,
    #[error("{0}")]
    Remote(RemoteError),
    #[error("{0}")]
    Git(String),
    #[error("the remote's answer was refused: {0}")]
    Rejected(String),
    #[error("commit {0} was not confirmed for building")]
    NotConfirmed(Commit),
    #[error("{0} is being built by another decanter")]
    Busy(Commit),
    #[error("{0}")]
    Download(#[from] download::Error),
    #[error("{0}")]
    Archive(#[from] archive::Error),
    #[error("the toolchain package's manifest doesn't match: {0}")]
    Manifest(String),
    #[error("could not confine the build: {0}")]
    Confinement(String),
    #[error("{step} failed: {detail}")]
    Tool { step: &'static str, detail: String },
    #[error("{step} ran past {seconds} seconds")]
    TimedOut { step: &'static str, seconds: u64 },
    #[error("{step} started {count} processes at once")]
    TooManyProcesses { step: &'static str, count: usize },
    #[error(
        "the checkout would be at {path}, {length} characters long; Windows builds need at most {MAX_CHECKOUT_PATH}: choose a shorter library location (Settings › Account › Change location)"
    )]
    PathTooLong { path: String, length: usize },
    #[error(
        "the toolchain's {0} changed after it was installed and checked; delete the toolchain in Ports to download it again"
    )]
    Tampered(String),
    #[error("the build's output: {0}")]
    Output(String),
    #[error("the built folder: {0}")]
    Package(package::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// A full commit as git prints it ([`prebuilt::valid_commit`]): it goes into git's arguments, a folder
/// name and a record.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Commit(String);

#[derive(Debug, thiserror::Error)]
#[error("not a full commit")]
pub struct InvalidCommit;

impl FromStr for Commit {
    type Err = InvalidCommit;

    fn from_str(commit: &str) -> Result<Self, Self::Err> {
        if prebuilt::valid_commit(commit) {
            Ok(Self(commit.to_owned()))
        } else {
            Err(InvalidCommit)
        }
    }
}

impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Commit {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The twelve characters psport's programs report (`PS_COMMIT`).
    #[must_use]
    pub fn short(&self) -> &str {
        self.0.get(..12).unwrap_or(&self.0)
    }
}

/// What a build is asked for: the branch of [`git::REMOTE`] and its commit, both as the remote
/// listed them and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub branch: String,
    pub commit: Commit,
}

/// Where a build stands, for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Downloading {
        name: String,
        received: u64,
        total: Option<u64>,
    },
    Unpacking(String),
    /// The installed toolchain's files hashed again; `total` is 0 until their lists are read.
    Verifying {
        checked: u32,
        total: u32,
    },
    Sources,
    Configuring,
    Compiling,
    Assembling,
}

/// A build in the library, as its record says it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub commit: Commit,
    pub branch: String,
    pub toolchain: u32,
    /// Seconds since the Unix epoch.
    pub built_at: u64,
}

#[derive(Serialize, Deserialize)]
struct Record {
    commit: String,
    remote: String,
    branch: String,
    toolchain: u32,
    toolchain_sha256: String,
    dlls: Vec<String>,
    built_at: u64,
}

#[derive(Serialize, Deserialize)]
struct Confirmation {
    commit: String,
    remote: String,
    branch: String,
    confirmed_at: u64,
}

/// A build's output: every line, cut at [`MAX_LINE_BYTES`], to its log file up to [`MAX_LOG_BYTES`],
/// and to the UI.
pub struct Log<'sink> {
    file: Option<tokio::fs::File>,
    written: u64,
    on_line: &'sink mut dyn FnMut(String),
}

impl<'sink> Log<'sink> {
    pub const fn new(file: Option<tokio::fs::File>, on_line: &'sink mut dyn FnMut(String)) -> Self {
        Self {
            file,
            written: 0,
            on_line,
        }
    }

    pub async fn line(&mut self, mut line: String) {
        if line.len() > MAX_LINE_BYTES {
            let mut end = MAX_LINE_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
            line.push_str(" [cut]");
        }
        let bytes = u64::try_from(line.len()).unwrap_or(u64::MAX).saturating_add(1);
        if self.written.saturating_add(bytes) > MAX_LOG_BYTES
            && let Some(mut file) = self.file.take()
        {
            let _ = file
                .write_all(b"decanter: the log ends here; the build's output went past its cap\n")
                .await;
        }
        if let Some(file) = self.file.as_mut() {
            if let Err(err) = file.write_all(format!("{line}\n").as_bytes()).await {
                tracing::warn!(%err, "could not write the Windows build's log; it goes on without it");
                self.file = None;
            }
            self.written = self.written.saturating_add(bytes);
        }
        (self.on_line)(line);
    }
}

/// Folders a build or an install leaves behind if it ends early, a cancel dropping its future
/// included, removed on a thread of their own so a large build folder never holds up the caller.
pub(crate) struct Leftovers(Vec<PathBuf>);

impl Leftovers {
    /// `paths`, files or folders, any of which may not exist yet.
    pub(crate) const fn new(paths: Vec<PathBuf>) -> Self {
        Self(paths)
    }
}

impl Drop for Leftovers {
    fn drop(&mut self) {
        let paths = std::mem::take(&mut self.0);
        let spawned = std::thread::Builder::new()
            .name("winbuild-cleanup".to_owned())
            .spawn(move || {
                for path in paths {
                    let removed = if path.is_dir() {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };
                    if let Err(err) = removed
                        && err.kind() != io::ErrorKind::NotFound
                    {
                        tracing::warn!(path = %path.display(), %err, "could not remove what a Windows build left");
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "could not start removing what a Windows build left");
        }
    }
}

/// Builds and packages share their folder: a commit can't be read as a `vX.Y.Z` tag.
fn builds(root: &Root) -> PathBuf {
    root.join("psport-windows")
}

/// The build of `commit`: `<library>/psport/psport-windows/<commit>`.
#[must_use]
pub fn dir(root: &Root, commit: &Commit) -> PathBuf {
    builds(root).join(commit.as_str())
}

fn record_path(root: &Root, commit: &Commit) -> PathBuf {
    builds(root).join(format!("{commit}.json"))
}

fn confirmed_path(root: &Root) -> PathBuf {
    builds(root).join("confirmed.json")
}

/// Where builds run and git asks the remote: `<library>/pw`, beside the `psport` folder rather than
/// in it, since every character counts against [`MAX_CHECKOUT_PATH`].
#[must_use]
pub fn work(root: &Root) -> PathBuf {
    root.library().join("pw")
}

fn work_name(commit: &Commit) -> &str {
    commit.as_str().get(..WORK_HEX).unwrap_or(commit.as_str())
}

/// `commit`'s checkout and build folders, `<library>/pw/<8 hex>/s` and `b`: as short as they can be
/// in the library, or the error saying how long they would be. Two commits sharing those 8 digits
/// share the folder's lock too, so one waits for the other ([`Error::Busy`]).
///
/// # Errors
/// When the checkout's path passes [`MAX_CHECKOUT_PATH`].
pub fn work_folder(root: &Root, commit: &Commit) -> Result<PathBuf, Error> {
    let folder = work(root).join(work_name(commit));
    let sources = folder.join("s").to_string_lossy().into_owned();
    let length = sources.chars().count();
    if length > MAX_CHECKOUT_PATH {
        return Err(Error::PathTooLong { path: sources, length });
    }
    Ok(folder)
}

/// The whole output of `commit`'s last build.
#[must_use]
pub fn log_path(root: &Root, commit: &Commit) -> PathBuf {
    work(root).join(format!("{commit}.log"))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Blocking file I/O.
fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= cap => std::fs::read(path).ok(),
        Ok(_) | Err(_) => None,
    }
}

async fn read_confirmations(root: &Root) -> Vec<Confirmation> {
    let path = confirmed_path(root);
    let bytes = match tokio::task::spawn_blocking(move || read_capped(&path, MAX_CONFIRMED_BYTES)).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) | Err(_) => return Vec::new(),
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|err| {
        tracing::warn!(%err, "unreadable list of confirmed Windows builds; asking again");
        Vec::new()
    })
}

/// The owner said yes to building `head`: kept, so the same commit isn't asked about again.
///
/// # Errors
/// When the list can't be written.
pub async fn confirm(root: &Root, head: &Head) -> io::Result<()> {
    let mut confirmations = read_confirmations(root).await;
    confirmations.retain(|confirmation| confirmation.commit != head.commit.as_str());
    confirmations.push(Confirmation {
        commit: head.commit.to_string(),
        remote: git::REMOTE.to_owned(),
        branch: head.branch.clone(),
        confirmed_at: now(),
    });
    let excess = confirmations.len().saturating_sub(MAX_CONFIRMED);
    confirmations.drain(..excess);
    let text = match serde_json::to_vec_pretty(&confirmations) {
        Ok(text) => text,
        Err(err) => return Err(io::Error::other(err)),
    };
    let path = confirmed_path(root);
    tracing::info!(commit = %head.commit, branch = head.branch, remote = git::REMOTE, "Windows build confirmed");
    match tokio::task::spawn_blocking(move || crate::files::write_atomic(&path, &text)).await {
        Ok(result) => result,
        Err(err) => Err(io::Error::other(err)),
    }
}

/// Whether the owner confirmed `commit` of [`git::REMOTE`].
pub async fn is_confirmed(root: &Root, commit: &Commit) -> bool {
    read_confirmations(root)
        .await
        .iter()
        .any(|confirmation| confirmation.commit == commit.as_str() && confirmation.remote == git::REMOTE)
}

/// `head` built and in the library, or the build already there. Refused unless [`confirm`] was
/// given for its commit and the toolchain is pinned; one build per commit at a time, across decanter
/// processes.
///
/// # Errors
/// When any step fails; the work folder goes, the log at [`log_path`] stays.
pub async fn build(
    root: &Root,
    head: &Head,
    on_stage: &mut dyn FnMut(Stage),
    on_line: &mut dyn FnMut(String),
) -> Result<Built, Error> {
    if let Some(built) = built_commit(root, &head.commit).await {
        return Ok(built);
    }
    if !is_confirmed(root, &head.commit).await {
        return Err(Error::NotConfirmed(head.commit.clone()));
    }
    if pins::package().is_none() {
        return Err(Error::NotPinned);
    }
    let folder = match work_folder(root, &head.commit) {
        Ok(folder) => folder,
        Err(err) => return Err(err),
    };
    let base = work(root);
    if let Err(err) = tokio::fs::create_dir_all(&base).await {
        return Err(err.into());
    }
    // Named as the work folder is, so two commits that would share it never run at once.
    let lock_path = base.join(format!("{}.lock", work_name(&head.commit)));
    let lock = match tokio::task::spawn_blocking({
        let (lock_path, builds, base, own) = (
            lock_path.clone(),
            builds(root),
            base.clone(),
            work_name(&head.commit).to_owned(),
        );
        move || {
            let lock = package::lock_install(&lock_path);
            if lock.is_ok() {
                sweep_staging(&builds, &base, &own);
            }
            lock
        }
    })
    .await
    {
        Ok(Ok(lock)) => lock,
        Ok(Err(err)) if err.kind() == io::ErrorKind::WouldBlock => return Err(Error::Busy(head.commit.clone())),
        Ok(Err(err)) => return Err(err.into()),
        Err(err) => return Err(io::Error::other(err).into()),
    };
    // Another decanter may have built it while this one waited for the lock.
    if let Some(built) = built_commit(root, &head.commit).await {
        return Ok(built);
    }
    let _leftovers = Leftovers::new(vec![folder.clone(), staging(root, &head.commit)]);
    let file = match tokio::fs::File::create(log_path(root, &head.commit)).await {
        Ok(file) => Some(file),
        Err(err) => {
            tracing::warn!(%err, "could not create the Windows build's log file");
            None
        }
    };
    let mut log = Log::new(file, on_line);
    log.line(format!(
        "decanter: building {remote} {branch} at {commit} with toolchain-win32-{number}",
        remote = git::REMOTE,
        branch = head.branch,
        commit = head.commit,
        number = pins::TOOLCHAIN.number
    ))
    .await;
    tracing::info!(commit = %head.commit, branch = head.branch, "building psport for Windows");
    let result = steps(root, head, &folder, on_stage, &mut log).await;
    match &result {
        Ok(built) => {
            log.line(format!(
                "decanter: built {} into {}",
                built.commit,
                dir(root, &built.commit).display()
            ))
            .await;
            tracing::info!(commit = %head.commit, "psport built for Windows");
        }
        Err(err) => {
            log.line(format!("decanter: the build failed: {err}")).await;
            tracing::warn!(commit = %head.commit, %err, "psport's Windows build failed");
        }
    }
    drop(lock);
    result
}

/// Where `commit`'s folder is put together before it is renamed into place.
fn staging(root: &Root, commit: &Commit) -> PathBuf {
    builds(root).join(format!(".{commit}.{}", std::process::id()))
}

/// Removes the staging folders (`.<commit>.<pid>`) of builds no process runs any more: their work
/// folder's lock is free, or is `own`, which this build holds. Best effort, said in the log.
/// Blocking file I/O.
fn sweep_staging(builds: &Path, base: &Path, own: &str) {
    let Ok(entries) = std::fs::read_dir(builds) else {
        return;
    };
    let pid = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some((commit, owner)) = name
            .to_str()
            .and_then(|name| name.strip_prefix('.'))
            .and_then(|rest| rest.split_once('.'))
        else {
            continue;
        };
        let (Ok(commit), Ok(owner)) = (commit.parse::<Commit>(), owner.parse::<u32>()) else {
            continue;
        };
        if owner == pid {
            continue;
        }
        let prefix = work_name(&commit);
        let held = prefix != own
            && std::fs::File::options()
                .write(true)
                .open(base.join(format!("{prefix}.lock")))
                .is_ok_and(|lock| matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        if held {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => tracing::info!(%commit, owner, "staging of an interrupted Windows build removed"),
            Err(err) => tracing::warn!(%commit, owner, %err, "could not remove an interrupted Windows build"),
        }
    }
}

async fn steps(
    root: &Root,
    head: &Head,
    folder: &Path,
    on_stage: &mut dyn FnMut(Stage),
    log: &mut Log<'_>,
) -> Result<Built, Error> {
    let tools = match toolchain::ensure(root, on_stage).await {
        Ok(tools) => tools,
        Err(err) => return Err(err),
    };
    let (sources, out, temp) = (folder.join("s"), folder.join("b"), folder.join("t"));
    on_stage(Stage::Sources);
    if tokio::fs::symlink_metadata(folder).await.is_ok() {
        tracing::info!(dir = %folder.display(), "removing the work folder an earlier build left");
    }
    let _ = tokio::fs::remove_dir_all(folder).await;
    for made in [&sources, &out, &temp] {
        if let Err(err) = tokio::fs::create_dir_all(made).await {
            return Err(err.into());
        }
    }
    if let Err(err) = git::checkout(&tools.git, &work(root), &sources, head, log).await {
        return Err(err);
    }
    let env = os::build_env(&temp);
    on_stage(Stage::Configuring);
    let configure = [
        "-S".into(),
        cmake_path(&sources).into(),
        "-B".into(),
        cmake_path(&out).into(),
        "-G".into(),
        "Ninja".into(),
        format!("-DCMAKE_MAKE_PROGRAM={}", cmake_path(&tools.ninja)).into(),
        format!(
            "-DCMAKE_TOOLCHAIN_FILE={}",
            cmake_path(&tools.package.join("psport-toolchain.cmake"))
        )
        .into(),
        format!("-DPS_LLVM_MINGW={}", cmake_path(&tools.llvm_mingw)).into(),
        "-DCMAKE_BUILD_TYPE=RelWithDebInfo".into(),
        "-DPS_LTO=OFF".into(),
        format!("-DPS_COMMIT={}", head.commit.short()).into(),
    ];
    if let Err(err) = run::run(
        run::Step {
            name: "configure",
            program: &tools.cmake,
            args: &configure,
            dir: folder,
            env: &env,
            bounds: CONFIGURE,
        },
        log,
    )
    .await
    {
        return Err(err);
    }
    on_stage(Stage::Compiling);
    let jobs = std::thread::available_parallelism().map_or(4, |count| count.get().min(MAX_JOBS));
    let compile = [
        "-C".into(),
        out.clone().into_os_string(),
        "-j".into(),
        jobs.to_string().into(),
    ];
    if let Err(err) = run::run(
        run::Step {
            name: "compile",
            program: &tools.ninja,
            args: &compile,
            dir: folder,
            env: &env,
            bounds: COMPILE,
        },
        log,
    )
    .await
    {
        return Err(err);
    }
    on_stage(Stage::Assembling);
    install(root, head, &tools, &sources, &out).await
}

/// CMake reads `\` in a path as an escape in some places; `/` means the same to Windows.
fn cmake_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The build in `out` made into [`dir`], then recorded: only a folder that passed
/// [`assemble::check`] ever gets there.
async fn install(
    root: &Root,
    head: &Head,
    tools: &toolchain::Tools,
    sources: &Path,
    out: &Path,
) -> Result<Built, Error> {
    let staging = staging(root, &head.commit);
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let assembled = {
        let (sources, out, staging, head) = (sources.to_owned(), out.to_owned(), staging.clone(), head.clone());
        let sysroot_bin = tools.package.join("clang64").join("bin");
        let toolchain = format!(
            "toolchain-win32-{} ({})",
            pins::TOOLCHAIN.number,
            pins::TOOLCHAIN.sha256
        );
        let expected = tools.sysroot_bin.clone();
        tokio::task::spawn_blocking(move || match assemble::patches(&sources, &staging) {
            Ok(()) => assemble::folder(&out, &sysroot_bin, &expected, &staging, &head, &toolchain),
            Err(err) => Err(err),
        })
        .await
    };
    let dlls = match assembled {
        Ok(Ok(dlls)) => dlls,
        Ok(Err(err)) => {
            let _ = tokio::fs::remove_dir_all(&staging).await;
            return Err(err);
        }
        Err(err) => return Err(io::Error::other(err).into()),
    };
    let target = dir(root, &head.commit);
    let _ = tokio::fs::remove_dir_all(&target).await;
    if let Err(err) = tokio::fs::rename(&staging, &target).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(err.into());
    }
    let built = Built {
        commit: head.commit.clone(),
        branch: head.branch.clone(),
        toolchain: pins::TOOLCHAIN.number,
        built_at: now(),
    };
    let record = Record {
        commit: head.commit.to_string(),
        remote: git::REMOTE.to_owned(),
        branch: head.branch.clone(),
        toolchain: built.toolchain,
        toolchain_sha256: pins::TOOLCHAIN.sha256.to_owned(),
        dlls,
        built_at: built.built_at,
    };
    let text = match serde_json::to_vec_pretty(&record) {
        Ok(text) => text,
        Err(err) => return Err(io::Error::other(err).into()),
    };
    let path = record_path(root, &head.commit);
    match tokio::task::spawn_blocking(move || crate::files::write_atomic(&path, &text)).await {
        Ok(Ok(())) => Ok(built),
        Ok(Err(err)) => Err(err.into()),
        Err(err) => Err(io::Error::other(err).into()),
    }
}

/// Every build in the library with a valid record and its programs in place, newest first.
pub async fn built(root: &Root) -> Vec<Built> {
    let Ok(mut entries) = tokio::fs::read_dir(builds(root)).await else {
        return Vec::new();
    };
    let mut found = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(commit) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|commit| commit.parse::<Commit>().ok())
        else {
            continue;
        };
        if let Some(built) = built_commit(root, &commit).await {
            found.push(built);
        }
    }
    found.sort_by_key(|built| std::cmp::Reverse(built.built_at));
    found
}

/// `commit`'s build when its record reads back as written and its programs are files of their own:
/// a record is written only after the folder passed every check.
pub async fn built_commit(root: &Root, commit: &Commit) -> Option<Built> {
    let path = record_path(root, commit);
    let bytes = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || read_capped(&path, MAX_RECORD_BYTES))
            .await
            .ok()
            .flatten()?
    };
    let record: Record = match serde_json::from_slice(&bytes) {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "unreadable record of a Windows build");
            return None;
        }
    };
    if record.commit != commit.as_str()
        || record.remote != git::REMOTE
        || !crate::os::linux::updater::valid_branch(&record.branch)
    {
        tracing::warn!(path = %path.display(), "record of a Windows build that doesn't match its name");
        return None;
    }
    let dir = dir(root, commit);
    for file in [crate::bbport::direct::EXECUTABLE, crate::bbport::direct::GPU_LIBRARY] {
        if !tokio::fs::symlink_metadata(dir.join(file))
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            return None;
        }
    }
    Some(Built {
        commit: commit.clone(),
        branch: record.branch,
        toolchain: record.toolchain,
        built_at: record.built_at,
    })
}

/// `commit`'s record first, so a folder half removed never reads as built.
///
/// # Errors
/// When either can't be removed; a missing one is not an error.
pub async fn remove(root: &Root, commit: &Commit) -> io::Result<()> {
    for result in [
        tokio::fs::remove_file(record_path(root, commit)).await,
        tokio::fs::remove_dir_all(dir(root, commit)).await,
    ] {
        match result {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}
