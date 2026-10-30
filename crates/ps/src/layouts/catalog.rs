//! The catalog on disk: `layouts/<pad|bbport-keys|anyps5-keys>/<id>.json`, one user layout per
//! file, so a change writes only its own file and two views can't overwrite each other's layouts.
//!
//! Made once from the single `layouts.json` decanter kept before, after a copy of it into
//! `backups/per-game-settings-<date>/`: the files are written into a scratch folder that is renamed
//! into place whole, so a catalog is either complete or absent. `layouts.json` is left as it was,
//! what an older decanter reads.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Deserialize;

use super::{KeyLayout, Kind, Store, choice};
use crate::bbport::pad::PadLayout;
use crate::mapping::Target;
use crate::paths::Root;

const LEGACY: &str = "layouts.json";
const MAX_ID: usize = 32;

/// Tells apart the scratch folders of two threads making the catalog at the same moment.
static SCRATCH: AtomicU32 = AtomicU32::new(0);

#[derive(Deserialize)]
struct Legacy {
    #[serde(default)]
    pad: Vec<PadLayout>,
    #[serde(default)]
    bbport_keys: Vec<KeyLayout>,
    #[serde(default)]
    anyps5_keys: Vec<KeyLayout>,
}

/// An id a user layout's file may be named after: never a shipped layout's, which is built from
/// code and would shadow it.
fn is_user_id(id: &str) -> bool {
    (1..=MAX_ID).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        && !PadLayout::builtins().iter().any(|layout| layout.id == id)
}

fn file(dir: &Path, kind: Kind, id: &str) -> PathBuf {
    dir.join(kind.dir()).join(format!("{id}.json"))
}

/// Oldest first, as the former file listed them: `user-2` before `user-10`.
fn order(id: &str) -> (u32, &str) {
    let number = id
        .strip_prefix("user-")
        .and_then(|number| number.parse().ok())
        .unwrap_or(u32::MAX);
    (number, id)
}

pub(super) fn load(root: &Root) -> io::Result<Store> {
    let dir = root.layouts();
    if !dir.is_dir() {
        migrate(root, &dir)?;
    }
    let mut store = Store::default();
    for kind in Kind::ALL {
        let folder = dir.join(kind.dir());
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        let mut ids: Vec<String> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let id = name.to_str()?.strip_suffix(".json")?;
                if is_user_id(id) {
                    Some(id.to_owned())
                } else {
                    tracing::warn!(folder = %folder.display(), file = %name.to_string_lossy(), "layout file skipped, its name is not a layout id");
                    None
                }
            })
            .collect();
        ids.sort_by(|a, b| order(a).cmp(&order(b)));
        for id in ids {
            let path = file(&dir, kind, &id);
            if let Err(err) = read_into(&mut store, kind, &path, id.clone()) {
                tracing::warn!(path = %path.display(), %err, "layout file skipped, it can't be read");
                store.unreadable.push((kind, id));
            }
        }
    }
    Ok(store)
}

fn read_into(store: &mut Store, kind: Kind, path: &Path, id: String) -> io::Result<()> {
    let data = fs::read(path)?;
    match kind {
        Kind::Pad => {
            let mut layout: PadLayout =
                serde_json::from_slice(decanter_core::bom::strip(&data, path)).map_err(io::Error::other)?;
            layout.id = id;
            store.pad.push(layout);
        }
        Kind::Keys(target) => {
            let mut layout: KeyLayout =
                serde_json::from_slice(decanter_core::bom::strip(&data, path)).map_err(io::Error::other)?;
            layout.id = id;
            store.keys_mut(target).push(layout);
        }
    }
    Ok(())
}

/// The catalog from the former `layouts.json`, when there is one. An id no file may be named after
/// (a hand edit) gets a new one, so no layout is lost.
fn migrate(root: &Root, dir: &Path) -> io::Result<()> {
    let legacy = root.join(LEGACY);
    let data = match fs::read(&legacy) {
        Ok(data) => data,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let old: Legacy = serde_json::from_slice(decanter_core::bom::strip(&data, &legacy))
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {err}", legacy.display())))?;
    let backup = crate::bbport::fresh_backup_dir(root, "layouts")?.join(LEGACY);
    fs::write(&backup, &data)?;
    let scratch = root.join(format!(
        "layouts.tmp-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed)
    ));
    let written = write_scratch(&scratch, old);
    let renamed = written.and_then(|count| fs::rename(&scratch, dir).map(|()| count));
    match renamed {
        Ok(count) => {
            tracing::info!(layouts = count, from = %legacy.display(), to = %dir.display(), backup = %backup.display(), "layouts catalog made from the former file");
            Ok(())
        }
        // Another thread or process made it first: the catalog in place is the one read.
        Err(_) if dir.is_dir() => {
            remove_scratch(&scratch);
            Ok(())
        }
        Err(err) => {
            remove_scratch(&scratch);
            Err(err)
        }
    }
}

fn remove_scratch(scratch: &Path) {
    if let Err(err) = fs::remove_dir_all(scratch)
        && err.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %scratch.display(), %err, "scratch layouts folder left behind");
    }
}

fn write_scratch(scratch: &Path, old: Legacy) -> io::Result<usize> {
    fs::create_dir_all(scratch)?;
    let mut store = Store::default();
    for mut layout in old.pad {
        if !is_user_id(&layout.id) || store.pad.iter().any(|known| known.id == layout.id) {
            layout.id = store.next_id(Kind::Pad);
        }
        write(&file(scratch, Kind::Pad, &layout.id), &layout)?;
        store.pad.push(layout);
    }
    for (target, layouts) in [(Target::Bbport, old.bbport_keys), (Target::AnyPs5, old.anyps5_keys)] {
        for mut layout in layouts {
            if !is_user_id(&layout.id) || store.keys(target).iter().any(|known| known.id == layout.id) {
                layout.id = store.next_id(Kind::Keys(target));
            }
            write(&file(scratch, Kind::Keys(target), &layout.id), &layout)?;
            store.keys_mut(target).push(layout);
        }
    }
    Ok(store.pad.len() + store.bbport_keys.len() + store.anyps5_keys.len())
}

fn write(path: &Path, layout: &impl serde::Serialize) -> io::Result<()> {
    let text = serde_json::to_string_pretty(layout).map_err(io::Error::other)? + "\n";
    crate::files::write_atomic(path, text.as_bytes())
}

/// The catalog's file for `id`, made from the former file first if it is still to be made, so a
/// write never leaves the former layouts behind.
fn target_file(root: &Root, kind: Kind, id: &str) -> io::Result<PathBuf> {
    if !is_user_id(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{id:?} is not a user layout's id"),
        ));
    }
    let dir = root.layouts();
    if !dir.is_dir() {
        migrate(root, &dir)?;
    }
    Ok(file(&dir, kind, id))
}

/// Writes one of the user's gamepad layouts. File I/O: off the UI thread.
///
/// # Errors
/// A shipped layout, which is never stored, or writing the file.
pub fn put_pad(layout: &PadLayout) -> io::Result<()> {
    write(&target_file(&crate::paths::active(), Kind::Pad, &layout.id)?, layout)
}

/// Writes one of the user's keyboard layouts for `target`. File I/O: off the UI thread.
///
/// # Errors
/// A shipped layout, which is never stored, or writing the file.
pub fn put_keys(target: Target, layout: &KeyLayout) -> io::Result<()> {
    write(
        &target_file(&crate::paths::active(), Kind::Keys(target), &layout.id)?,
        layout,
    )
}

/// Deletes one of the user's layouts after a copy into `backups/per-game-settings-<date>/`, and
/// drops it from every game that recorded it ([`choice::forget`]); those games keep their lines, so
/// they play as before. Returns how many games had recorded it. File I/O: off the UI thread.
///
/// # Errors
/// A shipped layout, backing up or deleting the file, or a game's file that could not be rewritten.
pub fn remove(root: &Root, kind: Kind, id: &str) -> io::Result<usize> {
    let path = target_file(root, kind, id)?;
    if path.is_file() {
        let backup = crate::bbport::fresh_backup_dir(root, "layouts")?
            .join(kind.dir())
            .join(format!("{id}.json"));
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&path, &backup)?;
        fs::remove_file(&path)?;
        tracing::info!(layout = id, kind = kind.dir(), backup = %backup.display(), "layout deleted");
    }
    choice::forget(root, kind, id)
}
