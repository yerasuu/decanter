// SPDX-License-Identifier: GPL-3.0-or-later
//! Where an Amazon game's files and records live in a library folder, and the SHA-256 checks of its
//! files; the link-safe joins and moves are the ones every store shares.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use decanter_runtime::install::files::component;
pub(super) use decanter_runtime::install::files::{
    has_size, inside, install_dir_name, move_into_place, prune_dirs, remove_files,
};
use sha2::{Digest as _, Sha256};

use crate::manifest::{Manifest, ManifestFile};

/// decanter's own folder for Amazon in a library root: staging and the saved manifests.
fn amazon_dir(root: &Path) -> PathBuf {
    root.join("decanter").join("amazon")
}

/// Every account's staged downloads in `root`, apart from GOG's `downloading/` so neither store's
/// scan reads the other's folders.
pub(super) fn downloading_dir(root: &Path) -> PathBuf {
    amazon_dir(root).join("downloading")
}

/// Where `account_id`'s download of `product_id` is staged in `root`: outside `common/`, so a
/// stopped download never leaves partial files where the game lives, and on its disk, so the
/// finished files move in by rename.
pub(super) fn staging_dir(root: &Path, account_id: &str, product_id: &str) -> io::Result<PathBuf> {
    super::LAYOUT.staging_dir(root, account_id, product_id)
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
pub(super) fn saved_manifest(root: &Path, product_id: &str) -> io::Result<PathBuf> {
    let name = component(product_id, "product id")?;
    let mut file = name.into_os_string();
    file.push(".raw");
    Ok(amazon_dir(root).join("manifests").join(file))
}

/// Whether `path` is a regular file (not a link) of `file`'s size and hash. Reads the whole file.
pub(super) fn matches(path: &Path, file: &ManifestFile) -> io::Result<bool> {
    if !has_size(path, file.size())? {
        return Ok(false);
    }
    Ok(sha256_of(path)? == *file.sha256().as_bytes())
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

/// Creates the folders the manifest lists, empty ones included, which some games expect to exist.
pub(super) fn create_dirs(dir: &Path, manifest: &Manifest) -> io::Result<()> {
    for folder in manifest.dirs() {
        std::fs::create_dir_all(inside(dir, folder)?)?;
    }
    Ok(())
}
