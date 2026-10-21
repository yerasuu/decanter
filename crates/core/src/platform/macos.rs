//! macOS: the platform folders `dirs` knows, `fs4` for disk space, Unix
//! links and modes, and what it supports so far.

use core::iter;
use std::fs::{self, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};

use crate::capabilities::{Feature, Reason, Support};

/// Device and inode: the same folder reached through two paths compares equal.
pub type FolderIdentity = (u64, u64);

/// Where `libraries.toml` goes: `~/Library/Application Support/decanter`.
#[must_use]
#[inline]
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("decanter"))
}

/// Where a Linux decanter kept everything before the library held it;
/// never written here, so the migration finds nothing to move.
#[must_use]
#[inline]
pub fn legacy_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("decanter"))
}

#[must_use]
#[inline]
pub fn default_library_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("decanter").join("steamapps"))
}

/// No per-session runtime folder here: the lock file sits in decanter's
/// local data folder instead, which it may have to create.
#[must_use]
#[inline]
pub fn lock_dir() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("decanter");
    fs::create_dir_all(&dir)
        .inspect_err(|err| tracing::warn!(dir = %dir.display(), %err, "cannot create the runtime lock's folder"))
        .ok()?;
    Some(dir)
}

/// `(free bytes, total bytes)` of the volume holding `path`.
///
/// # Errors
/// When `path` doesn't exist or its volume can't be asked.
#[inline]
pub fn disk_space(path: &Path) -> io::Result<(u64, u64)> {
    let stat = fs4::statvfs(path)?;
    Ok((stat.available_space(), stat.total_space()))
}

/// # Errors
/// Reading `folder`'s metadata.
#[inline]
pub fn folder_identity(folder: &Path) -> io::Result<FolderIdentity> {
    let metadata = fs::metadata(folder)?;
    Ok((metadata.dev(), metadata.ino()))
}

/// Nothing to ask without `/proc`, and nothing to protect either: the old
/// data folder only ever existed on Linux.
#[must_use]
#[inline]
pub const fn others_running() -> bool {
    false
}

/// Exported shortcuts start decanter's Proton launch, which only Linux has.
#[inline]
pub fn steam_root_candidates() -> impl Iterator<Item = PathBuf> {
    iter::empty()
}

/// # Errors
/// Creating the link.
#[inline]
pub fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
    symlink(target, link)
}

/// # Errors
/// Creating the link.
#[inline]
pub fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
    symlink(target, link)
}

/// What a downloaded program's file needs before it can run, if anything.
#[must_use]
#[inline]
pub fn executable_permissions() -> Option<Permissions> {
    Some(Permissions::from_mode(0o755))
}

/// Browsing the library, GOG and achievements only: no launch backend
/// exists here yet.
#[must_use]
#[inline]
pub const fn support(feature: Feature) -> Support {
    match feature {
        Feature::Play
        | Feature::Proton
        | Feature::Prefixes
        | Feature::ExternalInstaller
        | Feature::GalaxyHook
        | Feature::Tray
        | Feature::CloseToTray
        | Feature::PadClose
        | Feature::DesktopEntries
        | Feature::Scopes
        | Feature::SelfUpdate
        | Feature::SteamExport
        | Feature::PsSection
        | Feature::PsDownloads
        | Feature::PsPlay
        | Feature::PsMenuBinding
        | Feature::PsKeyStick
        | Feature::PsOnline
        | Feature::MangoHud
        | Feature::GameMode => Support::Unavailable(Reason::NotOnThisSystem),
    }
}
