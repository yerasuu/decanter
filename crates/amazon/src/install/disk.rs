// SPDX-License-Identifier: GPL-3.0-or-later
//! Where an Amazon game's files and records live in a library folder, and the file checks that never
//! follow a symbolic link: a manifest path is joined under the install directory only through
//! [`inside`], so a link planted in the game's folder cannot send a write or a delete elsewhere.

use std::collections::BTreeSet;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use decanter_runtime::sanitize_relative_path;
use sha2::{Digest as _, Sha256};

use crate::manifest::{Manifest, ManifestFile, ManifestPath};

/// nile's `ILLEGAL_FNAME_CHARS` (analysis, section 4.3), so a folder reads the same in both.
const ILLEGAL_NAME_CHARS: [char; 12] = [':', '/', '*', '?', '<', '>', '\\', '|', '™', '"', '®', '\0'];
const NAME_MAX_BYTES: usize = 255;

/// decanter's own folder for Amazon in a library root: staging and the saved manifests.
fn amazon_dir(root: &Path) -> PathBuf {
    root.join("decanter").join("amazon")
}

fn component(value: &str, what: &str) -> anyhow::Result<PathBuf> {
    sanitize_relative_path(value)
        .filter(|path| path.components().count() == 1)
        .with_context(|| format!("the {what} {value:?} cannot be a folder name"))
}

/// Every account's staged downloads in `root`, apart from GOG's `downloading/` so neither store's
/// scan reads the other's folders.
pub(super) fn downloading_dir(root: &Path) -> PathBuf {
    amazon_dir(root).join("downloading")
}

/// Where `account_id`'s download of `product_id` is staged in `root`: outside `common/`, so a
/// stopped download never leaves partial files where the game lives, and on its disk, so the
/// finished files move in by rename.
pub(super) fn staging_dir(root: &Path, account_id: &str, product_id: &str) -> anyhow::Result<PathBuf> {
    Ok(downloading_dir(root)
        .join(component(account_id, "account id")?)
        .join(component(product_id, "product id")?))
}

/// Amazon's SDK DLLs, laid out as the launcher's manifest names them and shared by every Amazon game
/// of the library folder.
pub(super) fn sdk_dir(root: &Path) -> PathBuf {
    amazon_dir(root).join("sdk")
}

pub(super) fn sdk_staging(root: &Path) -> PathBuf {
    amazon_dir(root).join("sdk-downloading")
}

/// Written once every SDK file is in place, with the launcher version they came from.
pub(super) fn sdk_marker(root: &Path) -> PathBuf {
    amazon_dir(root).join("sdk.version")
}

/// The manifest an install was made from, kept raw as nile keeps it, to plan the next update and
/// to verify and uninstall without asking Amazon.
pub(super) fn saved_manifest(root: &Path, product_id: &str) -> anyhow::Result<PathBuf> {
    let name = component(product_id, "product id")?;
    let mut file = name.into_os_string();
    file.push(".raw");
    Ok(amazon_dir(root).join("manifests").join(file))
}

/// The folder under `common/` for a game titled `title`: nile's illegal characters and control
/// characters removed, and trailing dots and spaces, which Windows drops from a name. Falls back to
/// `product_id` when nothing usable is left.
pub(super) fn install_dir_name(title: &str, product_id: &str) -> String {
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
        product_id.to_owned()
    } else {
        name.to_owned()
    }
}

/// `relative` under `dir`, refused when a folder on the way already exists as a symbolic link.
/// The last component is not checked: callers replace or delete a link there, never open it.
pub(super) fn inside(dir: &Path, relative: &ManifestPath) -> io::Result<PathBuf> {
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

/// Whether `path` is a regular file (not a link) of `file`'s size and hash. Reads the whole file.
pub(super) fn matches(path: &Path, file: &ManifestFile) -> io::Result<bool> {
    if !has_size(path, file.size())? {
        return Ok(false);
    }
    Ok(sha256_of(path)? == *file.sha256().as_bytes())
}

/// Whether `path` is a regular file (not a link) of `size` bytes.
pub(super) fn has_size(path: &Path, size: u64) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_file() && meta.len() == size),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

/// The SHA-256 state after the first bytes of `path`, so a resumed file is hashed whole.
pub(super) fn hasher_over(path: &Path) -> io::Result<Sha256> {
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        let Some(bytes) = buffer.get(..read).filter(|bytes| !bytes.is_empty()) else {
            return Ok(hasher);
        };
        hasher.update(bytes);
    }
}

fn sha256_of(path: &Path) -> io::Result<[u8; 32]> {
    Ok(hasher_over(path)?.finalize().into())
}

/// What deleting a manifest's files from a folder did.
#[derive(Debug, Default)]
pub(super) struct Removal {
    pub(super) removed: usize,
    pub(super) missing: usize,
    pub(super) failed: usize,
}

/// Deletes `paths` under `dir`, a symbolic link as the link itself; a path through a linked folder
/// is left and counted as failed.
pub(super) fn remove_files<'path>(dir: &Path, paths: impl IntoIterator<Item = &'path ManifestPath>) -> Removal {
    let mut removal = Removal::default();
    for relative in paths {
        let outcome = inside(dir, relative).and_then(std::fs::remove_file);
        match outcome {
            Ok(()) => removal.removed = removal.removed.saturating_add(1),
            Err(err) if err.kind() == io::ErrorKind::NotFound => removal.missing = removal.missing.saturating_add(1),
            Err(err) => {
                tracing::warn!(path = relative.as_str(), error = %err, "couldn't delete an amazon game file");
                removal.failed = removal.failed.saturating_add(1);
            }
        }
    }
    removal
}

/// Removes the folders of `paths` and `dirs` under `dir` that are left empty, deepest first; one
/// holding anything else (a save, a user's file) stays.
pub(super) fn prune_dirs<'path>(
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

/// Creates the folders the manifest lists, empty ones included, which some games expect to exist.
pub(super) fn create_dirs(dir: &Path, manifest: &Manifest) -> io::Result<()> {
    for folder in manifest.dirs() {
        std::fs::create_dir_all(inside(dir, folder)?)?;
    }
    Ok(())
}

/// Moves a finished file from staging over `destination`, replacing a link there rather than
/// writing through it; a library on another disk than staging gets a copy instead.
pub(super) fn move_into_place(staged: &Path, destination: &Path) -> io::Result<()> {
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
