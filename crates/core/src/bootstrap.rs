//! What decanter reads before it has a database to ask: which library is
//! active, and so where that database is.
//!
//! Everything decanter stores lives in the library root (`<root>/decanter/`,
//! beside `common/` and `compatdata/`). Outside it there is only
//! `libraries.toml` in the platform's config folder (`~/.config/decanter/`,
//! `%APPDATA%\decanter\`, `~/Library/Application Support/decanter/`), the
//! pointer to the roots, and the runtime lock that keeps a migration or a
//! library move from running under another decanter. Before the one-time
//! migration (see [`crate::migrate`]) the database still lives in the old
//! data directory, which [`Location::Legacy`]
//! keeps working until then.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use crate::platform;

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Libraries {
    #[serde(default)]
    known: Vec<PathBuf>,
    active: Option<PathBuf>,
    profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A library root, whether or not its disk is there right now: a root
    /// that's missing is never created, it reads as unavailable instead.
    Library(PathBuf),
    /// Not migrated yet: the database and caches are still in this folder.
    Legacy(PathBuf),
    /// No library yet: the first-run screen makes one.
    Unconfigured,
}

/// `--library`, which wins over everything else.
static OVERRIDE: OnceLock<PathBuf> = OnceLock::new();
/// Resolved on first use and again after anything rewrites the pointer.
static LOCATION: RwLock<Option<Location>> = RwLock::new(None);
/// The shared lock this process holds for its whole life once taken.
static HELD: OnceLock<File> = OnceLock::new();

#[must_use]
pub fn config_path() -> Option<PathBuf> {
    platform::config_dir().map(|dir| dir.join("libraries.toml"))
}

/// Where everything lived before it moved into the library.
#[must_use]
pub fn legacy_dir() -> Option<PathBuf> {
    platform::legacy_data_dir()
}

#[must_use]
pub fn legacy_db_path() -> Option<PathBuf> {
    legacy_dir().map(|dir| dir.join("links.db"))
}

/// decanter's own folder inside a library root.
#[must_use]
pub fn library_data_dir(root: &Path) -> PathBuf {
    root.join("decanter")
}

#[must_use]
pub fn db_path(root: &Path) -> PathBuf {
    library_data_dir(root).join("decanter.db")
}

/// Points this process at `root` regardless of `libraries.toml`, for
/// `--library`.
pub fn set_override(root: PathBuf) {
    let _ = OVERRIDE.set(root);
    forget();
}

#[must_use]
pub fn location() -> Location {
    let cached = LOCATION
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(location) = cached {
        return location;
    }
    let location = resolve();
    tracing::debug!(?location, "resolved the active library");
    *LOCATION.write().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(location.clone());
    location
}

fn forget() {
    *LOCATION.write().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

fn resolve() -> Location {
    if let Some(root) = OVERRIDE.get() {
        return Location::Library(root.clone());
    }
    if let Some(root) = std::env::var_os("DECANTER_LIBRARY").filter(|value| !value.is_empty()) {
        let root = PathBuf::from(root);
        if root.is_absolute() {
            return Location::Library(root);
        }
        tracing::warn!(root = %root.display(), "ignoring DECANTER_LIBRARY, it isn't an absolute path");
    }
    match read_config() {
        Ok(Some(libraries)) => return libraries.active.map_or(Location::Unconfigured, Location::Library),
        Ok(None) => {}
        Err(err) => {
            // Not the old database either: after the migration it's a stale
            // copy, and what got written to it would be lost.
            tracing::error!(%err, "cannot read libraries.toml");
            return Location::Unconfigured;
        }
    }
    match (legacy_dir(), legacy_db_path()) {
        (Some(dir), Some(db)) if db.is_file() => Location::Legacy(dir),
        _ => Location::Unconfigured,
    }
}

/// The active library's root while its disk is there. Before the migration
/// that's the old default library.
#[must_use]
pub fn active_root() -> Option<PathBuf> {
    match location() {
        Location::Library(root) => root.is_dir().then_some(root),
        Location::Legacy(dir) => Some(dir.join("steamapps")),
        Location::Unconfigured => None,
    }
}

/// The active library's root when it's configured but its disk isn't there.
#[must_use]
pub fn unavailable_root() -> Option<PathBuf> {
    match location() {
        Location::Library(root) => (!root.is_dir()).then_some(root),
        Location::Legacy(_) | Location::Unconfigured => None,
    }
}

/// Where decanter's own files for the active library go: the database,
/// the art cache, `umu-run`.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    match location() {
        Location::Library(root) => root.is_dir().then(|| library_data_dir(&root)),
        Location::Legacy(dir) => Some(dir),
        Location::Unconfigured => None,
    }
}

/// The active database, with its folder made if the root is there.
#[must_use]
pub fn active_db_path() -> Option<PathBuf> {
    match location() {
        Location::Library(root) if root.is_dir() => {
            let dir = library_data_dir(&root);
            if let Err(err) = fs::create_dir_all(&dir) {
                tracing::warn!(dir = %dir.display(), %err, "cannot create the library's data folder");
            }
            Some(db_path(&root))
        }
        Location::Legacy(dir) => Some(dir.join("links.db")),
        Location::Library(_) | Location::Unconfigured => None,
    }
}

/// Every library root `libraries.toml` lists.
#[must_use]
pub fn known_roots() -> Vec<PathBuf> {
    read_config()
        .ok()
        .flatten()
        .map(|libraries| libraries.known)
        .unwrap_or_default()
}

/// Makes `root` the active library, adding it to the known ones.
///
/// # Errors
/// Fails when `libraries.toml` can't be read or written.
pub fn activate(root: &Path) -> io::Result<()> {
    let mut libraries = read_config()?.unwrap_or_default();
    if !libraries.known.iter().any(|known| known == root) {
        libraries.known.push(root.to_path_buf());
    }
    libraries.active = Some(root.to_path_buf());
    write_config(&libraries)?;
    tracing::info!(root = %root.display(), "activated library");
    Ok(())
}

/// Records the profile last in use, once there's a `libraries.toml` to hold it.
pub fn remember_profile(profile_id: &str) {
    let result = read_config().and_then(|libraries| match libraries {
        Some(mut libraries) if libraries.profile.as_deref() != Some(profile_id) => {
            libraries.profile = Some(profile_id.to_owned());
            write_config(&libraries)
        }
        Some(_) | None => Ok(()),
    });
    if let Err(err) = result {
        tracing::warn!(profile_id, %err, "cannot record the active profile in libraries.toml");
    }
}

/// Follows a library root that moved, in the known list and as the active one.
///
/// # Errors
/// Fails when `libraries.toml` can't be read or written.
pub fn relocate(from: &Path, to: &Path) -> io::Result<()> {
    let Some(mut libraries) = read_config()? else {
        return Ok(());
    };
    for root in libraries.known.iter_mut().chain(libraries.active.as_mut()) {
        if root == from {
            *root = to.to_path_buf();
        }
    }
    write_config(&libraries)?;
    tracing::info!(from = %from.display(), to = %to.display(), "library pointer moved");
    Ok(())
}

/// Writes the pointer the migration ends with.
pub(crate) fn commit(known: Vec<PathBuf>, active: Option<PathBuf>, profile: Option<String>) -> io::Result<()> {
    write_config(&Libraries { known, active, profile })
}

fn read_config() -> io::Result<Option<Libraries>> {
    let Some(path) = config_path() else {
        return Ok(None);
    };
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    toml::from_str(crate::bom::strip_str(&text, &path))
        .map(Some)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Written aside, synced and renamed into place: this file is what says a
/// migration finished, so it's either there whole or not at all.
fn write_config(libraries: &Libraries) -> io::Result<()> {
    let path = config_path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no config directory"))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("/"));
    fs::create_dir_all(dir)?;
    let text = toml::to_string(libraries).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let partial = path.with_extension("toml.part");
    let mut file = File::create(&partial)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(&partial, &path)?;
    File::open(dir)?.sync_all()?;
    forget();
    Ok(())
}

fn open_lock() -> Option<File> {
    let path = platform::lock_dir()?.join("decanter.lock");
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .inspect_err(|err| tracing::warn!(path = %path.display(), %err, "cannot open the runtime lock"))
        .ok()
}

/// Takes the shared lock every running decanter holds, waiting out a
/// migration or library move in progress.
pub fn hold_shared() {
    let Some(file) = open_lock() else {
        return;
    };
    if let Err(err) = file.lock_shared() {
        tracing::warn!(%err, "cannot take the shared runtime lock");
        return;
    }
    let _ = HELD.set(file);
}

/// Runs `work` while no other decanter holds the runtime lock, or returns
/// `None` without running it when one does.
pub fn exclusive<T>(work: impl FnOnce() -> T) -> Option<T> {
    let Some(held) = HELD.get() else {
        let file = open_lock();
        if let Some(file) = &file
            && !try_exclusive(file)
        {
            return None;
        }
        return Some(work());
    };
    let locked = try_exclusive(held);
    let result = locked.then(work);
    // Converting a flock drops the old lock first, even when the new one
    // is refused, so the shared one is taken back either way.
    if let Err(err) = held.lock_shared() {
        tracing::warn!(%err, "cannot take the shared runtime lock back");
    }
    result
}

fn try_exclusive(file: &File) -> bool {
    match file.try_lock() {
        Ok(()) => true,
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Error(err)) => {
            tracing::warn!(%err, "cannot take the exclusive runtime lock");
            false
        }
    }
}
