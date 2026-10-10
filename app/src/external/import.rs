//! Moving a game that already lives in another tool's Wine prefix —
//! Lutris, Bottles, Heroic, Steam's Proton, or one made by hand — into
//! decanter: the prefix moves into this profile's `compatdata`, with
//! whatever was installed in it, its registry and its saves, and the game
//! becomes an external game like any other.
//!
//! Moved or copied, the user's choice: a move is a rename on the same disk
//! and costs nothing, but leaves the other tool's entry pointing at a
//! prefix that is gone; a copy keeps that tool working, at the cost of the
//! space — these prefixes are often tens of GB — and of two copies of the
//! saves that drift apart from then on.

use std::path::{Path, PathBuf};

pub use super::transfer::{Mode, Progress};

/// Drops what a failed copy for `id` left behind, once nothing will retry it.
pub fn abandon(profile_id: &str, id: &str) {
    // In whichever folder the form was adding it to.
    for root in crate::core::library_for_profile(profile_id)
        .map(|library| library.roots())
        .unwrap_or_default()
    {
        super::transfer::discard_staging(&root.join("compatdata").join(profile_id).join(id));
    }
}

/// Drops every failed copy in `profile_id`'s library — for when no form is
/// open on any of them.
pub fn abandon_all(profile_id: &str) {
    for root in crate::core::library_for_profile(profile_id)
        .map(|library| library.roots())
        .unwrap_or_default()
    {
        super::transfer::discard_all_staging(&root.join("compatdata").join(profile_id));
    }
}

/// The Wine prefix proper under `picked` — the directory holding `drive_c`.
/// Steam's Proton (and tools copying it) keeps that in a `pfx` subdirectory;
/// picking `drive_c` itself is taken to mean its parent.
pub fn wine_root(picked: &Path) -> Option<PathBuf> {
    if picked.file_name().is_some_and(|name| name == "drive_c") {
        return picked.parent().map(Path::to_path_buf);
    }
    [picked.to_path_buf(), picked.join("pfx")]
        .into_iter()
        .find(|root| root.join("drive_c").is_dir())
}

/// Brings the prefix `picked` names into `profile_id`'s library under `id`,
/// then writes `draft` as that game — its executable remapped if it was
/// inside the prefix. Blocking: a copy, or a move to another disk, copies
/// every byte.
pub fn import(
    profile_id: &str,
    id: &str,
    picked: &Path,
    mode: Mode,
    progress: &Progress,
    draft: super::Draft,
) -> Result<(), String> {
    let root = crate::achv_try!(wine_root(picked).ok_or("esa carpeta no es un prefijo de Wine (no tiene drive_c)"));
    if !decanter_runtime::Prefix::new(&root).processes().is_empty() {
        return Err("Hay algo corriendo en ese prefijo: cerralo primero.".to_owned());
    }
    let library = crate::achv_try!(super::library_in(profile_id, draft.library_root.as_deref()));
    let destination = crate::achv_try!(
        library
            .compatdata(profile_id, id)
            .ok_or("no se pudo resolver el prefijo nuevo")
    );
    // An installer may already have made one for this draft; the imported
    // prefix replaces it only if it's still empty.
    if destination.exists() && std::fs::read_dir(&destination).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err("Este juego ya tiene un prefijo propio; empezá uno nuevo para importar.".to_owned());
    }
    if let Some(parent) = destination.parent() {
        crate::achv_try!(std::fs::create_dir_all(parent).map_err(|err| format!("creando {}: {err}", parent.display())));
    }
    tracing::info!(profile_id, id, from = %root.display(), to = %destination.display(), copy = mode == Mode::Copy, "importing prefix");
    crate::achv_try!(
        super::transfer::bring(&root, &destination, mode, |done| progress.record(done))
            .inspect_err(|err| tracing::error!(from = %root.display(), err, "could not import prefix"))
    );
    let steam_layout = root != picked && root.parent() == Some(picked);
    if steam_layout {
        adopt_proton_files(picked, &destination, mode);
    }

    let executable = match draft.executable.strip_prefix(&root) {
        Ok(relative) => destination.join(relative),
        // Outside the prefix: it stays where it is.
        Err(_) => draft.executable,
    };
    let draft = super::Draft { executable, ..draft };
    if let Err(err) = super::save(profile_id, id, draft) {
        // Nothing points at the new prefix without its manifest: a moved
        // one goes back where the other tool expects it, a copy goes.
        let undo = match mode {
            Mode::Move => super::transfer::bring(&destination, &root, Mode::Move, |_| {}),
            Mode::Copy => std::fs::remove_dir_all(&destination).map_err(|err| err.to_string()),
        };
        if let Err(undo) = undo {
            tracing::error!(prefix = %destination.display(), %undo, "could not undo a failed import");
        }
        return Err(err);
    }
    tracing::info!(profile_id, id, "prefix imported");
    Ok(())
}

/// A Steam-style prefix keeps Proton's bookkeeping (`version`,
/// `tracked_files`, `config_info`) beside `pfx`, where Proton reads it to
/// tell an existing prefix from a new one. umu's layout keeps it beside
/// `drive_c`, so it comes in alongside; after a move, what's left (a lock
/// file) goes.
fn adopt_proton_files(old_root: &Path, destination: &Path, mode: Mode) {
    let Ok(entries) = std::fs::read_dir(old_root) else {
        return;
    };
    for entry in entries.flatten() {
        let target = destination.join(entry.file_name());
        let is_file = entry.file_type().is_ok_and(|kind| kind.is_file());
        if !is_file || target.exists() {
            continue;
        }
        let carried = match mode {
            Mode::Move => std::fs::rename(entry.path(), &target),
            Mode::Copy => std::fs::copy(entry.path(), &target).map(|_| ()),
        };
        if let Err(err) = carried {
            tracing::warn!(file = %entry.path().display(), %err, "could not carry a proton file over");
        }
    }
    if mode == Mode::Copy {
        return;
    }
    if let Err(err) = std::fs::remove_dir_all(old_root) {
        tracing::warn!(dir = %old_root.display(), %err, "could not remove what was left of the old prefix");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_drive_c_in_both_layouts() {
        let base = std::env::temp_dir().join(format!("decanter-import-test-{}", std::process::id()));
        let wine = base.join("lutris");
        let steam = base.join("steam");
        std::fs::create_dir_all(wine.join("drive_c")).expect("wine layout");
        std::fs::create_dir_all(steam.join("pfx/drive_c")).expect("steam layout");
        assert_eq!(wine_root(&wine), Some(wine.clone()));
        assert_eq!(wine_root(&steam), Some(steam.join("pfx")));
        assert_eq!(wine_root(&wine.join("drive_c")), Some(wine));
        assert_eq!(wine_root(&base), None);
        let _ = std::fs::remove_dir_all(&base);
    }
}
