//! Registering a game folder the user already has, as the Amazon and Epic installs do: the folder
//! is untrusted, so it is held to the shape an install record needs before any file of it is hashed
//! against the store's live manifest.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use decanter_core::Library;

use super::{BrokenFile, VerifyReport};

/// What to do with a folder whose files differ from the store's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Differences {
    /// Report them and record nothing.
    Refuse,
    /// Record the install anyway, for a repair to fetch what differs. Still refused when no file of
    /// the folder matches, since that is another game's folder or an empty one.
    Accept,
}

/// What an import found.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Imported {
    pub files_checked: usize,
    pub broken: Vec<BrokenFile>,
    /// Whether the install was recorded.
    pub is_recorded: bool,
}

impl Imported {
    /// What hashing the folder found, not recorded yet.
    #[inline]
    #[must_use]
    pub fn new(report: VerifyReport) -> Self {
        Self {
            files_checked: report.files_checked,
            broken: report.broken,
            is_recorded: false,
        }
    }
}

/// A folder an import may record.
#[derive(Debug)]
#[non_exhaustive]
pub struct ImportFolder {
    /// The library folder whose `common/` holds it.
    pub root: PathBuf,
    /// Its name under `common/`, which the install record names it by.
    pub install_dir: String,
    /// Its canonical path.
    pub dest: PathBuf,
}

/// Checks that `folder` can be recorded as an install: it must be a direct child of a library
/// folder's `common/`, since an install record names its folder by that single path component, and
/// one reached through a link is refused.
///
/// # Errors
///
/// The folder can't be read, is not a folder, is not directly in a library folder's `common/`, or
/// its name is not one an install record can use.
#[inline]
pub fn resolve_folder(library: &Library, folder: &Path) -> anyhow::Result<ImportFolder> {
    let dest = std::fs::canonicalize(folder).with_context(|| format!("reading {}", folder.display()))?;
    anyhow::ensure!(dest.is_dir(), "{} is not a folder", dest.display());
    let parent = dest.parent().context("the folder has no parent")?;
    let name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .context("the folder's name is not valid text")?
        .to_owned();
    let root = library
        .roots()
        .into_iter()
        .find(|root| std::fs::canonicalize(root.join("common")).is_ok_and(|common| common == parent))
        .with_context(|| format!("{} is not directly inside a library folder's common/", dest.display()))?;
    let usable = library
        .install_dir(&name)
        .and_then(|found| std::fs::canonicalize(found).ok());
    anyhow::ensure!(
        usable.as_deref() == Some(dest.as_path()),
        "the folder {name:?} cannot be used for a game install"
    );
    Ok(ImportFolder {
        root,
        install_dir: name,
        dest,
    })
}
