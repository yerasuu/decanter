// SPDX-License-Identifier: GPL-3.0-or-later
//! Where an Epic game's download and records live in a library folder, apart from GOG's and
//! Amazon's, and the SHA-1 checks of its files; the link-safe joins and moves are the ones every
//! store shares.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use decanter_runtime::install::files::component;
pub(super) use decanter_runtime::install::files::{
    has_size, inside, install_dir_name, move_into_place, prune_dirs, remove_files,
};

use crate::manifest::{ManifestFile, Sha1Digest};

/// decanter's own folder for Epic in a library root: staging and the saved manifests.
fn epic_dir(root: &Path) -> PathBuf {
    root.join("decanter").join("epic")
}

/// Every account's staged downloads in `root`.
pub(super) fn downloading_dir(root: &Path) -> PathBuf {
    epic_dir(root).join("downloading")
}

pub(super) fn staging_dir(root: &Path, account_id: &str, app_name: &str) -> io::Result<PathBuf> {
    super::LAYOUT.staging_dir(root, account_id, app_name)
}

/// The manifest an install was made from, kept as downloaded, to plan the next update and to
/// verify and uninstall without asking Epic.
pub(super) fn saved_manifest(root: &Path, app_name: &str) -> io::Result<PathBuf> {
    let mut file = component(app_name, "app name")?.into_os_string();
    file.push(".manifest");
    Ok(epic_dir(root).join("manifests").join(file))
}

/// Whether `path` is a regular file (not a link) of `file`'s size and SHA-1. Reads the whole file.
pub(super) fn matches(path: &Path, file: &ManifestFile) -> io::Result<bool> {
    if !has_size(path, file.size())? {
        return Ok(false);
    }
    Ok(sha1_of(path)? == *file.sha1())
}

pub(super) fn sha1_of(path: &Path) -> io::Result<Sha1Digest> {
    let mut hasher = sha1_smol::Sha1::new();
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        let Some(bytes) = buffer.get(..read).filter(|bytes| !bytes.is_empty()) else {
            return Ok(Sha1Digest(hasher.digest().bytes()));
        };
        hasher.update(bytes);
    }
}
