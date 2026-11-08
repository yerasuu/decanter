//! The file steps every store's install takes on a game's folder, none of which follows a symbolic
//! link: a manifest path is joined under the install directory only through [`inside`], so a link
//! planted in the game's folder cannot send a write or a delete elsewhere.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use super::path::ManifestPath;
use crate::sanitize_relative_path;

/// nile's `ILLEGAL_FNAME_CHARS`, which legendary's install folders strip too, so a folder reads the
/// same in decanter and in both.
const ILLEGAL_NAME_CHARS: [char; 12] = [':', '/', '*', '?', '<', '>', '\\', '|', '™', '"', '®', '\0'];
const NAME_MAX_BYTES: usize = 255;

/// `value` as a single folder name, or why it can't be one.
///
/// # Errors
///
/// When `value` is empty, climbs out of its base or holds more than one component.
#[inline]
pub fn component(value: &str, what: &str) -> io::Result<PathBuf> {
    sanitize_relative_path(value)
        .filter(|path| path.components().count() == 1)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the {what} {value:?} cannot be a folder name"),
            )
        })
}

/// The folder under `common/` for a game titled `title`: the illegal characters and control
/// characters removed, and trailing dots and spaces, which Windows drops from a name. Falls back to
/// `game_id` when nothing usable is left.
#[inline]
#[must_use]
pub fn install_dir_name(title: &str, game_id: &str) -> String {
    let cleaned: String = title
        .chars()
        .filter(|c| !ILLEGAL_NAME_CHARS.contains(c) && !c.is_control())
        .collect();
    let mut name = cleaned.trim().trim_end_matches(['.', ' ']).to_owned();
    while name.len() > NAME_MAX_BYTES {
        name.pop();
    }
    let name = name.trim_end_matches(['.', ' ']);
    if name.is_empty() || name == "." || name == ".." {
        game_id.to_owned()
    } else {
        name.to_owned()
    }
}

/// Where a file being fetched is written until it is whole and verified.
#[inline]
#[must_use]
pub fn part_path(done: &Path) -> PathBuf {
    let mut part = done.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// `relative` under `dir`, refused when a folder on the way already exists as a symbolic link.
/// The last component is not checked: callers replace or delete a link there, never open it.
///
/// # Errors
///
/// A folder on the way is a link, or can't be looked at.
#[inline]
pub fn inside(dir: &Path, relative: &ManifestPath) -> io::Result<PathBuf> {
    joined(dir, relative.as_path())
}

/// [`inside`] for a path already known to be one of a manifest's, or a folder of one.
fn joined(dir: &Path, relative: &Path) -> io::Result<PathBuf> {
    let mut path = dir.to_path_buf();
    let mut components = relative.components().peekable();
    let mut is_present = true;
    while let Some(part) = components.next() {
        path.push(part);
        if components.peek().is_none() || !is_present {
            continue;
        }
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is a symbolic link inside the game's folder", path.display()),
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => is_present = false,
            Err(err) => return Err(err),
        }
    }
    Ok(path)
}

/// Whether `path` is a regular file (not a link) of `size` bytes.
///
/// # Errors
///
/// `path` can't be looked at for another reason than not being there.
#[inline]
pub fn has_size(path: &Path, size: u64) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_file() && meta.len() == size),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// What deleting a manifest's files from a folder did.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Removal {
    pub removed: usize,
    pub missing: usize,
    pub failed: usize,
}

/// Deletes `paths` under `dir`, a symbolic link as the link itself; a path through a linked folder
/// is left and counted as failed.
#[inline]
pub fn remove_files<'path>(dir: &Path, paths: impl IntoIterator<Item = &'path ManifestPath>) -> Removal {
    let mut removal = Removal::default();
    for relative in paths {
        let outcome = inside(dir, relative).and_then(std::fs::remove_file);
        match outcome {
            Ok(()) => removal.removed = removal.removed.saturating_add(1),
            Err(err) if err.kind() == io::ErrorKind::NotFound => removal.missing = removal.missing.saturating_add(1),
            Err(err) => {
                tracing::warn!(path = relative.as_str(), error = %err, "couldn't delete a game file");
                removal.failed = removal.failed.saturating_add(1);
            }
        }
    }
    removal
}

/// Removes the folders of `paths` and `dirs` under `dir` that are left empty, deepest first; one
/// holding anything else (a save, a user's file) stays.
#[inline]
pub fn prune_dirs<'path>(
    dir: &Path,
    paths: impl IntoIterator<Item = &'path ManifestPath>,
    dirs: &'path [ManifestPath],
) -> usize {
    let mut folders = BTreeSet::new();
    for path in paths.into_iter().chain(dirs) {
        let text = path.as_str();
        folders.extend(text.match_indices('/').filter_map(|(end, _)| text.get(..end)));
    }
    folders.extend(dirs.iter().map(ManifestPath::as_str));
    let mut ordered: Vec<&str> = folders.into_iter().collect();
    ordered.sort_by_key(|folder| core::cmp::Reverse(folder.matches('/').count()));
    ordered
        .into_iter()
        .filter(|folder| {
            // A link, on the way or as the folder itself, is not one of the game's folders: removing
            // through it would reach outside the game's.
            joined(dir, Path::new(folder)).is_ok_and(|path| {
                std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_dir())
                    && std::fs::remove_dir(&path).is_ok()
            })
        })
        .count()
}

/// Moves a finished file from staging over `destination`, replacing a link there rather than
/// writing through it; a library on another disk than staging gets a copy instead.
///
/// # Errors
///
/// A folder stands where the file goes, or the move and the copy both fail.
#[inline]
pub fn move_into_place(staged: &Path, destination: &Path) -> io::Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::symlink_metadata(destination) {
        Ok(meta) if meta.file_type().is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is a folder where the game has a file", destination.display()),
            ));
        }
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(destination)?,
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    if std::fs::rename(staged, destination).is_ok() {
        return Ok(());
    }
    // A rename across disks always fails the same way, so its error says nothing the copy's won't.
    std::fs::copy(staged, destination)?;
    if let Err(err) = std::fs::remove_file(staged) {
        tracing::warn!(path = %staged.display(), error = %err, "couldn't remove a staged file after copying it into place");
    }
    Ok(())
}
