//! Windows: the known folders `dirs` knows, `fs4` for disk space, file
//! and directory links told apart, no modes, and what it supports so far.

use std::fs::{self, Permissions};
use std::os::windows::fs as windows_fs;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, io};

use crate::GameSystem;
use crate::capabilities::{Feature, Reason, Source, Support};

/// `CREATE_NO_WINDOW`, so `reg` doesn't flash a console over decanter.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The canonical path: the same folder reached through two paths compares equal.
pub type FolderIdentity = PathBuf;

/// Where `libraries.toml` goes: `%APPDATA%\decanter`.
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

/// Under the local app data rather than the roaming one, so games don't
/// roam with the user's profile.
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
/// Resolving `folder`.
#[inline]
pub fn folder_identity(folder: &Path) -> io::Result<FolderIdentity> {
    fs::canonicalize(folder)
}

/// Nothing to ask without `/proc`, and nothing to protect either: the old
/// data folder only ever existed on Linux.
#[must_use]
#[inline]
pub const fn others_running() -> bool {
    false
}

/// The user's default UI locale as a BCP 47 tag (`pt-BR`): no POSIX
/// locale variable is set for a program started from the desktop here.
#[must_use]
#[inline]
pub fn system_locale() -> Option<String> {
    sys_locale::get_locale()
}

/// What Steam's installer records first, then its default folders.
#[inline]
pub fn steam_root_candidates() -> impl Iterator<Item = PathBuf> {
    let default_folders = ["ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(env::var_os)
        .map(|dir| PathBuf::from(dir).join("Steam"));
    registered_steam_path().into_iter().chain(default_folders)
}

/// `SteamPath` under `HKEY_CURRENT_USER\Software\Valve\Steam`, read with
/// `reg` because the registry has no safe binding in the dependencies.
/// Only an absolute path is taken, since it ends up under `userdata`.
fn registered_steam_path() -> Option<PathBuf> {
    let output = Command::new("reg")
        .args(["query", r"HKCU\Software\Valve\Steam", "/v", "SteamPath"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .inspect_err(|err| tracing::debug!(%err, "could not ask the registry for Steam's folder"))
        .ok()?;
    let listing = String::from_utf8_lossy(&output.stdout);
    let path = PathBuf::from(listing.lines().find_map(|line| {
        let (name, rest) = line.trim().split_once("REG_SZ")?;
        (name.trim() == "SteamPath").then(|| rest.trim())
    })?);
    path.is_absolute().then_some(path)
}

/// # Errors
/// Creating the link, which needs Developer Mode or the privilege for it.
#[inline]
pub fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
    windows_fs::symlink_dir(target, link)
}

/// # Errors
/// Creating the link, which needs Developer Mode or the privilege for it.
#[inline]
pub fn symlink_file(target: &Path, link: &Path) -> io::Result<()> {
    windows_fs::symlink_file(target, link)
}

/// Games run as the Windows programs they are.
#[must_use]
#[inline]
pub const fn game_system() -> GameSystem {
    GameSystem::Windows
}

/// None: Windows runs a file by its extension, not a mode.
#[must_use]
#[inline]
pub const fn executable_permissions() -> Option<Permissions> {
    None
}

/// Games run natively, so Proton and everything around it isn't here; the
/// Linux desktop's pieces aren't ported yet.
///
/// The PS section plays psport's Windows package, downloaded from its releases,
/// or a psport.exe from a folder the user picks: nothing is compiled here yet,
/// and no online play. It, the tray, the close-game
/// shortcut and the Steam export wait for a run on a real Windows.
#[must_use]
#[inline]
pub const fn support(feature: Feature) -> Support {
    match feature {
        Feature::GalaxyHook if !cfg!(target_arch = "x86_64") => Support::Unavailable(Reason::NotOnThisSystem),
        Feature::Play | Feature::ExternalInstaller | Feature::GalaxyHook => Support::Available,
        Feature::Tray
        | Feature::CloseToTray
        | Feature::PadClose
        | Feature::SteamExport
        | Feature::PsSection
        | Feature::PsDownloads
        | Feature::PsPlay => Support::Testing,
        Feature::PsBuilds => Support::Unavailable(Reason::DownloadsInstead(Source::Psport)),
        Feature::Proton
        | Feature::Prefixes
        | Feature::DesktopEntries
        | Feature::Scopes
        | Feature::SelfUpdate
        | Feature::PsMenuBinding
        | Feature::PsKeyStick
        | Feature::PsOnline
        | Feature::MangoHud
        | Feature::GameMode => Support::Unavailable(Reason::NotOnThisSystem),
    }
}
