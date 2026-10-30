//! Files and folders only their owner reads: the database, the logs and the error reports, which
//! hold account ids, paths and what a game printed. The library may sit on a disk every local
//! user can read (another mount, an SD card), so its own `0700` home does not protect it.
//!
//! On Unix they are created `0600` and `0700`, and [`narrow`] takes older ones down to that at
//! startup. A filesystem that keeps its own modes (exFAT, NTFS, many network shares) refuses or
//! ignores the change, which is fine: its mount options decide for every file on it. On Windows
//! nothing is set: an ACL takes Win32 calls the workspace's `unsafe_code = forbid` rules out, and
//! a file inherits its folder's, which for the default library (`%LOCALAPPDATA%\decanter`) already
//! limits it to the user, SYSTEM and Administrators. A library the user put elsewhere (another
//! drive's root) has that folder's ACL, often readable by every local account.

#[cfg(unix)]
use std::fs;
use std::fs::{DirBuilder, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

#[cfg(unix)]
const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const SHARED_BITS: u32 = 0o077;

/// Options whose `create` makes a file only its owner reads; one that exists keeps its mode.
#[cfg(unix)]
#[must_use]
pub fn file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.mode(FILE_MODE);
    options
}

/// The file takes its folder's ACL: see the module's note on Windows.
#[cfg(not(unix))]
#[must_use]
pub fn file_options() -> OpenOptions {
    OpenOptions::new()
}

/// `dir` and its missing parents, those it creates only for their owner; folders that exist keep
/// their mode until [`narrow`].
///
/// # Errors
/// Creating a folder.
pub fn create_dir(dir: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(DIR_MODE);
    builder.create(dir)
}

/// Makes `path` an empty owner-only file when it is missing, so that what opens it next
/// (`SQLite`) finds the mode it copies to its journals instead of creating the file with the
/// default one. A file it cannot make is left to that opener, which says why.
pub fn create_file(path: &Path) {
    match file_options().write(true).create_new(true).open(path) {
        Ok(_) => tracing::debug!(path = %path.display(), "created as an owner-only file"),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => tracing::debug!(path = %path.display(), %err, "could not create as an owner-only file"),
    }
}

/// Takes `root` and everything under it down to its owner's bits (`0644` to `0600`, `0755` to
/// `0700`), without following links inside it. Once at startup, for what older versions made with
/// the default modes; logged once for `root`, never failing, since a mode it cannot change is no
/// reason to stop decanter.
#[cfg(unix)]
pub fn narrow(root: &Path) {
    let mut pending = vec![(root.to_path_buf(), true)];
    let (mut narrowed, mut refused) = (0_usize, 0_usize);
    let mut failed = None;
    while let Some((path, is_root)) = pending.pop() {
        let found = if is_root {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        };
        let metadata = match found {
            Ok(metadata) if metadata.file_type().is_symlink() => continue,
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                failed.get_or_insert(err);
                continue;
            }
        };
        if metadata.is_dir() {
            match fs::read_dir(&path) {
                Ok(entries) => pending.extend(entries.flatten().map(|entry| (entry.path(), false))),
                Err(err) => {
                    failed.get_or_insert(err);
                }
            }
        }
        match narrow_one(&path, metadata.mode()) {
            Ok(Some(true)) => narrowed = narrowed.saturating_add(1),
            Ok(Some(false)) => refused = refused.saturating_add(1),
            Ok(None) => {}
            Err(err) => {
                failed.get_or_insert(err);
            }
        }
    }
    if narrowed > 0 {
        tracing::warn!(path = %root.display(), narrowed, "files or folders were readable by other users, made owner-only");
    }
    if refused > 0 {
        tracing::debug!(path = %root.display(), refused, "the filesystem keeps its own modes, left as they are");
    }
    if let Some(err) = failed {
        tracing::warn!(path = %root.display(), %err, "could not make every file or folder owner-only");
    }
}

/// `None` when `path` was already owner-only, else whether its filesystem took the new mode.
#[cfg(unix)]
fn narrow_one(path: &Path, st_mode: u32) -> io::Result<Option<bool>> {
    let mode = st_mode & 0o7777;
    if mode & SHARED_BITS == 0 {
        return Ok(None);
    }
    match fs::set_permissions(path, fs::Permissions::from_mode(mode & !SHARED_BITS)) {
        // Some filesystems take the call and keep their mode.
        Ok(()) => Ok(Some(fs::metadata(path)?.mode() & SHARED_BITS == 0)),
        Err(err) if is_fixed_mode(&err) => Ok(Some(false)),
        Err(err) => Err(err),
    }
}

/// Nothing to narrow: see the module's note on Windows.
#[cfg(not(unix))]
pub const fn narrow(_root: &Path) {}

/// `EPERM` from exFAT or NTFS, `ENOTSUP` from a network share or FUSE: the mount decides modes.
#[cfg(unix)]
fn is_fixed_mode(err: &io::Error) -> bool {
    #[cfg(target_os = "macos")]
    const ENOTSUP: i32 = 45;
    #[cfg(not(target_os = "macos"))]
    const ENOTSUP: i32 = 95;
    matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported | io::ErrorKind::ReadOnlyFilesystem
    ) || err.raw_os_error() == Some(ENOTSUP)
}
