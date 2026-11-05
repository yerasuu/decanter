//! Steam's own list of library folders, `<steam_root>/steamapps/libraryfolders.vdf`: read for the
//! folders Steam keeps games in, and merged into so that Steam also lists decanter's.
//!
//! Steam owns every entry it wrote and every field in them (`contentid`, `totalsize`, `apps`, ...),
//! so a write only adds entries of decanter's (`path` and an empty `label`, under the next free
//! index) and only takes out entries decanter added. The format has no room to mark an entry as
//! decanter's that Steam would keep on its next save, so the caller keeps that record and hands it
//! in as `own`. Steam holds the list in memory while it runs and saves it back over the file, so a
//! change made then is staged beside it, the same way as `shortcuts.vdf`'s (see [`super::export`]).

#![expect(
    clippy::std_instead_of_alloc,
    reason = "desktop app, no no_std target exists to justify alloc:: here"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::{fs, io};

use super::process::{self, SteamState};
use crate::vdf::{self, Value};

const ROOT_KEY: &str = "libraryfolders";

/// A library folder Steam lists, the folder holding its `steamapps`.
pub struct LibraryFolder {
    /// Absolute, with no `..` in it.
    pub path: PathBuf,
    /// `false` while its disk (an SD card, an external drive) isn't mounted.
    pub is_present: bool,
}

/// What [`register`] changed in the list Steam reads next.
#[derive(Default)]
pub struct Registration {
    pub added: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Steam was running (or couldn't be ruled out), so the change waits in the staged copy.
    pub is_staged: bool,
}

fn real_path(steam_root: &Path) -> PathBuf {
    steam_root.join("steamapps").join("libraryfolders.vdf")
}

fn pending_path(steam_root: &Path) -> PathBuf {
    real_path(steam_root).with_extension("vdf.pending")
}

/// Whether a staged change to the library folders waits for Steam to restart.
#[must_use]
#[inline]
pub fn has_pending(steam_root: &Path) -> bool {
    pending_path(steam_root).is_file()
}

/// Every folder Steam lists right now, its own install among them (Steam lists it first), in the
/// file's order. A path that isn't absolute or climbs with `..` is left out, since the file sits
/// where users edit it. Empty when there is no file or it can't be parsed.
#[must_use]
#[inline]
pub fn library_folders(steam_root: &Path) -> Vec<LibraryFolder> {
    let path = real_path(steam_root);
    let root = match read(&path) {
        Ok(Some(root)) => root,
        Ok(None) => {
            tracing::debug!(path = %path.display(), "no libraryfolders.vdf to read");
            return Vec::new();
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "failed to read libraryfolders.vdf");
            return Vec::new();
        }
    };
    let mut folders: Vec<LibraryFolder> = Vec::new();
    let mut skipped = 0_usize;
    for (_, entry) in ordered(&root) {
        let Some(listed) = entry.get("path") else {
            continue;
        };
        let folder = PathBuf::from(listed);
        if !is_usable(&folder) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        if folders.iter().any(|known| known.path == folder) {
            continue;
        }
        folders.push(LibraryFolder {
            is_present: folder.join("steamapps").is_dir(),
            path: folder,
        });
    }
    if skipped > 0 {
        tracing::warn!(skipped, "libraryfolders.vdf lists paths that aren't absolute, left out");
    }
    tracing::debug!(count = folders.len(), "read steam's library folders");
    folders
}

fn is_usable(path: &Path) -> bool {
    path.is_absolute() && !path.components().any(|component| component == Component::ParentDir)
}

/// Brings the list Steam reads next in line with `wanted`: each folder in it that Steam doesn't
/// list yet and whose `steamapps` is there is added, and each folder in `own` (the ones decanter
/// added before) that isn't in `wanted` is taken out. Steam's own install is never added. Nothing
/// is written when nothing changes, so an unchanged list stages nothing under a running Steam.
///
/// A change staged earlier lands first when Steam has closed since. The entries left after a
/// removal are renumbered in the order they had, the same way Steam's
/// own Storage settings leave them, since a gap in the indices is something Steam never writes.
///
/// # Errors
/// Fails if the file exists but isn't a valid `libraryfolders` list (it is never replaced then), or
/// a write fails. A Steam with no `libraryfolders.vdf` gets none written.
#[inline]
pub fn register(steam_root: &Path, own: &[PathBuf], wanted: &[PathBuf]) -> io::Result<Registration> {
    let is_running = process::state(steam_root) != SteamState::NotRunning;
    let pending = pending_path(steam_root);
    // Steam closed since a change was staged: it lands now, so the staged copy can't go stale.
    if !is_running {
        apply_pending(steam_root, own)?;
    }
    let source = if is_running && pending.is_file() {
        pending.clone()
    } else {
        real_path(steam_root)
    };
    let Some(mut root) = read(&source)? else {
        tracing::debug!(path = %source.display(), "no libraryfolders.vdf, steam's library folders left alone");
        return Ok(Registration::default());
    };
    let steam_own = fs::canonicalize(steam_root).unwrap_or_else(|_| steam_root.to_path_buf());
    let removed = remove_entries(&mut root, |path| {
        own.iter().any(|mine| mine == path) && !wanted.iter().any(|kept| kept == path)
    });
    let mut added = Vec::new();
    for path in wanted {
        let is_steam_own = fs::canonicalize(path).is_ok_and(|path| path == steam_own);
        if is_steam_own || lists(&root, path) || !path.join("steamapps").is_dir() {
            continue;
        }
        let Some(text) = path.to_str() else {
            tracing::warn!(path = %path.display(), "library folder path isn't UTF-8, not listed in steam");
            continue;
        };
        let mut entry = Value::block();
        entry.insert("path", text);
        entry.insert("label", "");
        root.insert(next_index(&root).to_string(), entry);
        added.push(path.clone());
    }
    if added.is_empty() && removed.is_empty() {
        return Ok(Registration::default());
    }
    let target = if is_running { pending } else { real_path(steam_root) };
    write(&target, &root)?;
    tracing::info!(
        added = added.len(),
        removed = removed.len(),
        staged = is_running,
        "updated steam's library folders"
    );
    Ok(Registration {
        added,
        removed,
        is_staged: is_running,
    })
}

/// Moves the staged change into the real file, once Steam has exited: what Steam saved on its way
/// out, without the entries in `own` the staged copy no longer has, plus the ones in `own` it has
/// and Steam's file doesn't. Every other entry is Steam's as it saved it, since the user may have
/// added or removed a folder in Steam after the change was staged. A no-op when nothing is staged.
pub(super) fn apply_pending(steam_root: &Path, own: &[PathBuf]) -> io::Result<()> {
    let pending = pending_path(steam_root);
    let Some(staged) = read(&pending)? else {
        return Ok(());
    };
    let real = real_path(steam_root);
    let Some(mut saved) = read(&real)? else {
        // Nothing of Steam's to merge into: a file of decanter's alone would read as Steam's list.
        tracing::warn!(path = %real.display(), "libraryfolders.vdf is gone, staged library folders dropped");
        return fs::remove_file(&pending);
    };
    let is_own = |path: &Path| own.iter().any(|mine| mine == path);
    remove_entries(&mut saved, |path| is_own(path) && !lists(&staged, path));
    for (_, entry) in ordered(&staged) {
        let Some(path) = entry.get("path").map(Path::new) else {
            continue;
        };
        if is_own(path) && !lists(&saved, path) {
            saved.insert(next_index(&saved).to_string(), entry.clone());
        }
    }
    write(&real, &saved)?;
    fs::remove_file(&pending)
}

fn lists(root: &Value, path: &Path) -> bool {
    ordered(root).any(|(_, entry)| entry.get("path").is_some_and(|listed| Path::new(listed) == path))
}

/// Takes out the entries whose `path` matches, renumbering the rest when any went, and returns the
/// paths taken out.
fn remove_entries(root: &mut Value, matches: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Value::Block(block) = root else {
        return Vec::new();
    };
    let mut removed = Vec::new();
    block.retain(|key, entry| {
        let Some(path) = index(key).and_then(|_| entry.get("path")).map(PathBuf::from) else {
            return true;
        };
        if matches(&path) {
            removed.push(path);
            return false;
        }
        true
    });
    if removed.is_empty() {
        return removed;
    }
    let mut numbered: Vec<(u64, String)> = block
        .keys()
        .filter_map(|key| Some((index(key)?, key.clone())))
        .collect();
    numbered.sort_unstable();
    for (position, (_, key)) in numbered.into_iter().enumerate() {
        if let Some(entry) = block.remove(&key) {
            block.insert(position.to_string(), entry);
        }
    }
    removed
}

/// An entry's index, written the one way Steam writes it: decimal digits with no leading zero, so
/// two keys never stand for the same index.
fn index(key: &str) -> Option<u64> {
    let is_canonical = key.bytes().all(|byte| byte.is_ascii_digit()) && (key == "0" || !key.starts_with('0'));
    if !is_canonical || key.is_empty() {
        return None;
    }
    key.parse().ok()
}

fn next_index(root: &Value) -> u64 {
    root.as_block()
        .into_iter()
        .flat_map(BTreeMap::keys)
        .filter_map(|key| index(key))
        .max()
        .map_or(0, |last| last.saturating_add(1))
}

/// The entries in index order, other keys first: `vdf`'s blocks keep keys sorted as strings, which
/// puts `"10"` before `"2"`.
fn ordered(root: &Value) -> impl Iterator<Item = (&String, &Value)> {
    let mut entries: Vec<_> = root
        .as_block()
        .into_iter()
        .flatten()
        .filter(|(_, entry)| entry.as_block().is_some())
        .collect();
    entries.sort_by_key(|(key, _)| index(key).map_or((0, 0), |number| (1, number)));
    entries.into_iter()
}

/// `None` for a missing file: a Steam that never saved its list.
fn read(path: &Path) -> io::Result<Option<Value>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let invalid = |reason: String| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a valid library folder list: {reason}", path.display()),
        )
    };
    let (key, root) = vdf::from_str(crate::bom::strip_str(&text, path)).map_err(|err| invalid(err.to_string()))?;
    if !key.eq_ignore_ascii_case(ROOT_KEY) || root.as_block().is_none() {
        return Err(invalid(format!("root key {key:?}")));
    }
    Ok(Some(root))
}

/// Entries in index order, each as `vdf` writes a block one level down; written aside and renamed
/// into place, so Steam never reads half a file.
fn write(path: &Path, root: &Value) -> io::Result<()> {
    let mut text = format!("\"{ROOT_KEY}\"\n{{\n");
    let leaves = root
        .as_block()
        .into_iter()
        .flatten()
        .filter(|(_, value)| value.as_block().is_none());
    for (key, value) in leaves.chain(ordered(root)) {
        for line in vdf::to_string(key, value).lines() {
            text.push('\t');
            text.push_str(line);
            text.push('\n');
        }
    }
    text.push_str("}\n");
    let partial = path.with_extension("vdf.tmp");
    fs::write(&partial, text)?;
    fs::rename(&partial, path)
}
