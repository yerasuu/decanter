//! Linux: XDG folders, `/proc`, `statvfs`, Unix links and modes, and the
//! reference capability table.

use std::ffi::OsStr;
use std::fs::{self, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::{env, process};

use nix::sys::statvfs::statvfs;

use crate::capabilities::{Feature, Reason, Support};

/// Device and inode: the same folder reached through two paths compares equal.
pub type FolderIdentity = (u64, u64);

/// Where `libraries.toml` goes: `$XDG_CONFIG_HOME/decanter`.
#[must_use]
#[inline]
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("decanter"))
}

/// Where everything lived before it moved into the library:
/// `$XDG_DATA_HOME/decanter`.
#[must_use]
#[inline]
pub fn legacy_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("decanter"))
}

/// The old default library's place, which the first-run screen suggests.
#[must_use]
#[inline]
pub fn default_library_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("decanter").join("steamapps"))
}

/// The session's runtime folder, gone at logout with the lock in it.
#[must_use]
#[inline]
pub fn lock_dir() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
}

/// `(free bytes, total bytes)` of the filesystem holding `path`.
///
/// # Errors
/// `statvfs`'s, when `path` doesn't exist or can't be reached.
#[inline]
pub fn disk_space(path: &Path) -> io::Result<(u64, u64)> {
    let stat = statvfs(path)?;
    let block = stat.fragment_size();
    Ok((
        stat.blocks_available().saturating_mul(block),
        stat.blocks().saturating_mul(block),
    ))
}

/// # Errors
/// Reading `folder`'s metadata.
#[inline]
pub fn folder_identity(folder: &Path) -> io::Result<FolderIdentity> {
    let metadata = fs::metadata(folder)?;
    Ok((metadata.dev(), metadata.ino()))
}

/// Whether a decanter, or a game one started (it runs in a
/// `decanter.game.*` scope), is running for this same data folder — the
/// one this process would migrate. Anyone that can't be checked counts.
#[must_use]
#[inline]
pub fn others_running() -> bool {
    let own = process::id().to_string();
    let data_home = dirs::data_dir();
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let is_pid = name.as_bytes().iter().all(u8::is_ascii_digit);
        if !is_pid || name.as_bytes() == own.as_bytes() {
            return false;
        }
        let dir = entry.path();
        let ours = fs::read_to_string(dir.join("comm")).is_ok_and(|comm| comm.trim_end() == "decanter")
            || fs::read_to_string(dir.join("cgroup")).is_ok_and(|cgroup| cgroup.contains("decanter.game."));
        ours && same_data_home(&dir, data_home.as_deref())
    })
}

/// Whether `process` resolves the same `$XDG_DATA_HOME` as this one; an
/// environment that can't be read counts as the same.
fn same_data_home(process: &Path, data_home: Option<&Path>) -> bool {
    let Ok(environ) = fs::read(process.join("environ")) else {
        return true;
    };
    let var = |name: &[u8]| {
        environ
            .split(|byte| *byte == 0)
            .find_map(|entry| entry.strip_prefix(name)?.strip_prefix(b"="))
            .map(|value| PathBuf::from(OsStr::from_bytes(value)))
    };
    let theirs = var(b"XDG_DATA_HOME")
        .filter(|dir| dir.is_absolute())
        .or_else(|| var(b"HOME").map(|home| home.join(".local").join("share")));
    theirs.is_none_or(|theirs| Some(theirs.as_path()) == data_home)
}

/// Native installs first, Flatpak last since it's the least common on a Deck.
#[inline]
pub fn steam_root_candidates() -> impl Iterator<Item = PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let xdg_data_home = env::var_os("XDG_DATA_HOME").map(PathBuf::from);
    [
        xdg_data_home.map(|dir| dir.join("Steam")),
        home.clone().map(|dir| dir.join(".local/share/Steam")),
        home.clone().map(|dir| dir.join(".steam/steam")),
        home.map(|dir| dir.join(".var/app/com.valvesoftware.Steam/.local/share/Steam")),
    ]
    .into_iter()
    .flatten()
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

/// The reference system: everything is built for it first. What a given
/// machine lacks is taken away later, in [`crate::capabilities`].
#[must_use]
#[inline]
pub const fn support(feature: Feature) -> Support {
    match feature {
        Feature::PsDownloads if !cfg!(target_arch = "x86_64") => Support::Unavailable(Reason::NotOnThisSystem),
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
        | Feature::GameMode => Support::Available,
    }
}
