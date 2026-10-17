//! Bringing a whole prefix across safely: never a half-written prefix
//! where the game will look for one.
//!
//! The bytes land in a `.<name>.partial` directory beside the destination
//! and it only takes the destination's name, in one rename, once all of
//! them are there. rsync does the copying when it's installed — it checks
//! every file against a checksum as it writes it, keeps links, hard links,
//! permissions and times, and a retry of the same import picks up where
//! an interrupted one stopped. Without it, decanter's own copy fills the
//! same staging directory from scratch.
//!
//! A staging directory only lives as long as the form that could retry
//! into it: leaving the form, or opening the page afresh, deletes any.

use std::io::{self, BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use std::sync::atomic::{AtomicU64, Ordering};

use decanter_core::MoveProgress;
use rust_i18n::t;

/// How far a copy has got, written from the import's blocking thread and
/// read by the UI on its own timer —
/// an import's, or a game moving between library folders. Stays at zero for a same-disk move,
/// which is a rename with nothing to count.
#[derive(Default)]
pub struct Progress {
    copied: AtomicU64,
    total: AtomicU64,
}

impl Progress {
    /// `(copied, total)` bytes, or `None` before the total is known — the
    /// size walk of a big prefix alone takes a while.
    pub fn snapshot(&self) -> Option<(u64, u64)> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| (self.copied.load(Ordering::Relaxed), total))
    }

    pub fn record(&self, progress: MoveProgress) {
        self.total.store(progress.total_bytes, Ordering::Relaxed);
        self.copied.store(progress.bytes_copied, Ordering::Relaxed);
    }
}

/// Why a prefix could not be brought across: logged as it is, worded for the UI by
/// [`Error::localized`], which is also what it becomes as a view's `String` error.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{} is not empty: {err}", path.display())]
    NotEmpty { path: PathBuf, err: io::Error },
    #[error("rename into place failed: {0}")]
    Place(io::Error),
    #[error("measuring {} failed: {err}", path.display())]
    Measure { path: PathBuf, err: io::Error },
    #[error("removing {} failed: {err}", path.display())]
    Clean { path: PathBuf, err: io::Error },
    #[error("copy failed: {0}")]
    Copy(io::Error),
    #[error("rsync failed: {0}")]
    Rsync(String),
}

impl Error {
    pub fn localized(&self) -> String {
        match self {
            Self::NotEmpty { path, err } => {
                t!("external.transfer.errors.not_empty", path = path.display(), error = err)
            }
            Self::Place(err) => t!("external.transfer.errors.place", error = err),
            Self::Measure { path, err } => t!("external.transfer.errors.measure", path = path.display(), error = err),
            Self::Clean { path, err } => t!("external.transfer.errors.clean", path = path.display(), error = err),
            Self::Copy(err) => t!("external.transfer.errors.copy", error = err),
            Self::Rsync(err) => t!("external.transfer.errors.rsync", error = err),
        }
        .into_owned()
    }
}

impl From<Error> for String {
    fn from(err: Error) -> Self {
        err.localized()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Only an imported prefix is ever copied, and nothing is imported on Windows.
    #[cfg(not(windows))]
    Copy,
    Move,
}

/// Brings `src` to `dst`, which must not exist yet (or be empty). A move on
/// one filesystem is a rename; anything else is a copy, and for a move
/// `src` is removed only once the copy is whole.
pub fn bring(src: &Path, dst: &Path, mode: Mode, on_progress: impl FnMut(MoveProgress)) -> Result<(), Error> {
    if dst.is_dir() {
        // Left by an installer run that wrote nothing; it would block the
        // rename below.
        crate::achv_try!(std::fs::remove_dir(dst).map_err(|err| Error::NotEmpty {
            path: dst.to_owned(),
            err
        }));
    }
    if mode == Mode::Move && std::fs::rename(src, dst).is_ok() {
        tracing::info!(from = %src.display(), to = %dst.display(), "prefix moved (rename)");
        return Ok(());
    }
    let staging = staging_for(dst);
    crate::achv_try!(copy(src, &staging, on_progress));
    crate::achv_try!(std::fs::rename(&staging, dst).map_err(Error::Place));
    if mode == Mode::Move
        && let Err(err) = std::fs::remove_dir_all(src)
    {
        // The game is already whole at `dst`; this only leaves the old copy
        // taking space.
        tracing::warn!(dir = %src.display(), %err, "copied the prefix but could not remove the original");
    }
    tracing::info!(from = %src.display(), to = %dst.display(), moved = mode == Mode::Move, "prefix brought over (copy)");
    Ok(())
}

/// Deletes `dst`'s staging directory, if an import left one behind — off
/// the calling thread, since it can be tens of GB.
#[cfg(not(windows))]
pub fn discard_staging(dst: &Path) {
    let staging = staging_for(dst);
    if staging.exists() {
        remove_in_background(staging);
    }
}

/// Deletes every staging directory directly under `dir` — a profile's
/// `compatdata`, where only an import still being set up could use one.
#[cfg(not(windows))]
pub fn discard_all_staging(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with('.') && name.ends_with(".partial")
        })
        .for_each(|entry| remove_in_background(entry.path()));
}

#[cfg(not(windows))]
fn remove_in_background(staging: PathBuf) {
    std::thread::spawn(move || match std::fs::remove_dir_all(&staging) {
        Ok(()) => tracing::info!(dir = %staging.display(), "removed an abandoned prefix copy"),
        Err(err) => tracing::warn!(dir = %staging.display(), %err, "could not remove an abandoned prefix copy"),
    });
}

fn staging_for(dst: &Path) -> PathBuf {
    let name = dst
        .file_name()
        .map_or_else(Default::default, |name| name.to_string_lossy().into_owned());
    dst.with_file_name(format!(".{name}.partial"))
}

fn copy(src: &Path, staging: &Path, mut on_progress: impl FnMut(MoveProgress)) -> Result<(), Error> {
    let total_bytes = crate::achv_try!(decanter_core::directory_size(src).map_err(|err| Error::Measure {
        path: src.to_owned(),
        err
    }));
    on_progress(MoveProgress {
        bytes_copied: 0,
        total_bytes,
    });
    match rsync(src, staging, total_bytes, &mut on_progress) {
        Err(RsyncError::Missing) => {
            tracing::info!("rsync not installed, copying the prefix with the built-in copy");
            // The built-in copy can't resume: whatever an earlier attempt
            // left would only collide with it.
            if staging.exists() {
                crate::achv_try!(std::fs::remove_dir_all(staging).map_err(|err| Error::Clean {
                    path: staging.to_owned(),
                    err
                }));
            }
            decanter_core::copy_dir(src, staging, on_progress).map_err(Error::Copy)
        }
        Err(RsyncError::Failed(err)) => Err(Error::Rsync(err)),
        Ok(()) => Ok(()),
    }
}

enum RsyncError {
    /// Not installed: the built-in copy takes over.
    Missing,
    Failed(String),
}

fn rsync(
    src: &Path,
    staging: &Path,
    total_bytes: u64,
    on_progress: &mut impl FnMut(MoveProgress),
) -> Result<(), RsyncError> {
    // Trailing slashes: copy the contents of `src` into `staging`, not
    // `src` itself as a subdirectory.
    let from = format!("{}/", src.display());
    let to = format!("{}/", staging.display());
    let mut child = match Command::new("rsync")
        // -a keeps links as links, permissions, times; -H hard links; -S
        // sparse files. --partial-dir keeps an interrupted file for the
        // next attempt to resume instead of starting it over.
        .args([
            "-aHS",
            "--no-inc-recursive",
            "--info=progress2",
            "--partial-dir=.rsync-partial",
            "--",
        ])
        .arg(&from)
        .arg(&to)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Err(RsyncError::Missing),
        Err(err) => return Err(RsyncError::Failed(err.to_string())),
    };
    tracing::info!(from, to, "copying prefix with rsync");
    // Drained on its own thread: a full stderr pipe would stall rsync
    // while this one is still reading stdout.
    let stderr = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        })
    });
    if let Some(stdout) = child.stdout.take() {
        // progress2 rewrites one line with `\r`: "  1,234,567  12%  ...".
        for chunk in BufReader::new(stdout).split(b'\r').map_while(Result::ok) {
            if let Some(bytes_copied) = transferred(&chunk) {
                on_progress(MoveProgress {
                    bytes_copied: bytes_copied.min(total_bytes),
                    total_bytes,
                });
            }
        }
    }
    let status = child.wait().map_err(|err| RsyncError::Failed(err.to_string()))?;
    let stderr = stderr.and_then(|thread| thread.join().ok()).unwrap_or_default();
    if status.success() {
        return Ok(());
    }
    tracing::error!(%status, stderr = stderr.trim(), "rsync failed");
    let last = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    Err(RsyncError::Failed(format!("{status}: {}", last.trim())))
}

/// The byte count leading one progress2 update.
fn transferred(chunk: &[u8]) -> Option<u64> {
    let line = std::str::from_utf8(chunk).ok()?;
    let first = line.split_whitespace().next()?;
    first.replace(',', "").parse().ok()
}
