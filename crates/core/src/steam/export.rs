//! Merge-write orchestration for one Steam `shortcuts.vdf` entry and its
//! grid artwork — the only place that actually touches the real Steam
//! user's files.
//!
//! Never network-aware: callers hand over already-downloaded local image
//! files. Never destructive on read: an unparseable existing
//! `shortcuts.vdf` is a hard error here, not silently replaced with an
//! empty one — losing entries the user added by hand through Steam's own
//! UI is worse than failing this sync for one game.

#![expect(
    clippy::std_instead_of_alloc,
    reason = "desktop app, no no_std target exists to justify alloc:: here"
)]
#![expect(
    clippy::std_instead_of_core,
    reason = "desktop app, no no_std target exists to justify core:: here"
)]
#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "several functions here cost more than the 512-byte threshold by design, same as crate::vdf"
)]

use std::path::{Path, PathBuf};
use std::{fs, io};

use super::appid::{grid_appid, legacy_appid};
use super::binary_vdf::{self, Value};
use super::paths;

/// What a shortcut entry needs beyond its artwork.
pub struct ShortcutSpec<'spec> {
    pub name: &'spec str,
    pub exe: &'spec Path,
    pub start_dir: &'spec Path,
}

/// Already-downloaded local image files, one per Steam artwork slot.
/// `icon` becomes the entry's own `icon` field (the list-view icon); the
/// rest land under the grid directory, keyed by the *grid* appid (see
/// [`grid_appid`], distinct from the plain `appid` field). Absent fields
/// are simply skipped — Steam shows no art for that slot rather than
/// erroring.
#[derive(Default)]
pub struct Artwork {
    pub icon: Option<PathBuf>,
    pub capsule: Option<PathBuf>,
    pub portrait: Option<PathBuf>,
    pub hero: Option<PathBuf>,
}

/// Tag every decanter-written entry carries, so the user can tell
/// decanter's own shortcuts apart from ones they added by hand.
const TAG: &str = "Decanter";

/// Writes or updates this game's shortcut entry and places `artwork`
/// under the grid dir, returning the legacy appid to persist.
///
/// An entry already present for this exact appid is replaced in place
/// (same position in the file); otherwise a new one is appended. Entries
/// this call didn't create are never touched.
///
/// # Errors
/// Fails if `shortcuts.vdf` exists but isn't valid binary VDF, or a file
/// write fails.
#[inline]
pub fn sync(steam_root: &Path, user_id: &str, spec: &ShortcutSpec<'_>, artwork: &Artwork) -> io::Result<i32> {
    let exe_field = quote(&spec.exe.to_string_lossy());
    let start_dir_field = quote(&spec.start_dir.to_string_lossy());
    let appid = legacy_appid(&exe_field, spec.name);
    let grid_id = grid_appid(appid);

    let icon_dest = place_grid_image(steam_root, user_id, grid_id, "_icon", artwork.icon.as_deref())?;
    place_grid_image(steam_root, user_id, grid_id, "", artwork.capsule.as_deref())?;
    place_grid_image(steam_root, user_id, grid_id, "p", artwork.portrait.as_deref())?;
    place_grid_image(steam_root, user_id, grid_id, "_hero", artwork.hero.as_deref())?;

    let mut entry = Value::map();
    entry.push("appid", appid);
    entry.push("AppName", spec.name);
    entry.push("Exe", exe_field);
    entry.push("StartDir", start_dir_field);
    entry.push(
        "icon",
        icon_dest.map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
    );
    entry.push("ShortcutPath", "");
    entry.push("LaunchOptions", "");
    entry.push("IsHidden", 0);
    entry.push("AllowDesktopConfig", 1);
    entry.push("AllowOverlay", 1);
    entry.push("OpenVR", 0);
    entry.push("Devkit", 0);
    entry.push("DevkitGameID", "");
    entry.push("DevkitOverrideAppID", 0);
    entry.push("LastPlayTime", 0);
    entry.push("FlatpakAppID", "");
    let mut tags = Value::map();
    tags.push("0", TAG);
    entry.push("tags", tags);

    let path = paths::shortcuts_vdf_path(steam_root, user_id);
    let mut root = read_shortcuts(&path)?;
    upsert_entry(&mut root, appid, entry);
    write_shortcuts(&path, &root)?;

    Ok(appid)
}

/// Removes the shortcut entry for `appid` (a no-op if it's already gone)
/// and any grid artwork files under its [`grid_appid`], remaining entries
/// renumbered 0..N-1 the same way Steam's own UI leaves them after a
/// manual removal.
///
/// # Errors
/// Fails if `shortcuts.vdf` exists but isn't valid binary VDF, or the
/// rewritten file can't be written.
#[inline]
pub fn remove(steam_root: &Path, user_id: &str, appid: i32) -> io::Result<()> {
    let path = paths::shortcuts_vdf_path(steam_root, user_id);
    let mut root = read_shortcuts(&path)?;
    if let Value::Map(entries) = &mut root {
        entries.retain(|(_, entry)| entry.get("appid").and_then(Value::as_int) != Some(appid));
        renumber(entries);
    }
    write_shortcuts(&path, &root)?;

    let grid_id = grid_appid(appid);
    let dir = paths::grid_dir(steam_root, user_id);
    for stem_suffix in ["", "p", "_hero", "_icon"] {
        remove_any_extension(&dir, &format!("{grid_id}{stem_suffix}"));
    }

    Ok(())
}

fn quote(value: &str) -> String {
    format!("\"{value}\"")
}

/// Reads `shortcuts.vdf`'s entries as a flat, insertion-ordered map — a
/// missing file reads as empty (a Steam profile that's never added a
/// non-Steam game has none yet).
fn read_shortcuts(path: &Path) -> io::Result<Value> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Value::map()),
        Err(err) => return Err(err),
    };
    let (_root_key, root) = binary_vdf::decode(&bytes).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "existing shortcuts.vdf at {} is not valid binary VDF: {err}",
                path.display()
            ),
        )
    })?;
    Ok(root)
}

/// Writes via a temp file + rename — atomic, so a crash mid-write can't
/// leave a half-written file behind for Steam to read.
fn write_shortcuts(path: &Path, root: &Value) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = binary_vdf::encode("shortcuts", root);
    let tmp_path = path.with_extension("vdf.tmp");
    fs::write(&tmp_path, &bytes)?;
    fs::rename(&tmp_path, path)
}

fn upsert_entry(root: &mut Value, appid: i32, entry: Value) {
    let Value::Map(entries) = root else {
        return;
    };
    if let Some(slot) = entries
        .iter_mut()
        .find(|(_, existing)| existing.get("appid").and_then(Value::as_int) == Some(appid))
    {
        slot.1 = entry;
    } else {
        let index = entries.len().to_string();
        entries.push((index, entry));
    }
}

fn renumber(entries: &mut [(String, Value)]) {
    for (index, (key, _)) in entries.iter_mut().enumerate() {
        *key = index.to_string();
    }
}

/// Copies `source` into the grid dir as `{grid_id}{suffix}.<ext>`, `<ext>`
/// taken from `source`'s own extension (falls back to `jpg`, GOG's own
/// predominant format). `None` in, `None` out — nothing to place.
fn place_grid_image(
    steam_root: &Path,
    user_id: &str,
    grid_id: u64,
    suffix: &str,
    source: Option<&Path>,
) -> io::Result<Option<PathBuf>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let dir = paths::grid_dir(steam_root, user_id);
    fs::create_dir_all(&dir)?;
    let ext = source.extension().and_then(|ext| ext.to_str()).unwrap_or("jpg");
    let dest = dir.join(format!("{grid_id}{suffix}.{ext}"));
    fs::copy(source, &dest)?;
    Ok(Some(dest))
}

/// Removes every file in `dir` named `{stem}.<anything>` — the extension
/// isn't known at removal time (whatever it was when placed), so this
/// matches on the stem alone rather than guessing.
fn remove_any_extension(dir: &Path, stem: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.strip_prefix(stem).is_some_and(|rest| rest.starts_with('.')) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'spec>(exe: &'spec Path, start_dir: &'spec Path) -> ShortcutSpec<'spec> {
        ShortcutSpec {
            name: "Test Game",
            exe,
            start_dir,
        }
    }

    #[test]
    fn sync_then_sync_again_replaces_in_place_not_appends() {
        let dir = std::env::temp_dir().join(format!("decanter-steam-export-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let steam_root = dir.join("Steam");
        let exe = dir.join("game.exe");
        let start_dir = dir.clone();

        let appid_1 = sync(&steam_root, "1", &spec(&exe, &start_dir), &Artwork::default()).expect("first sync");
        let appid_2 = sync(&steam_root, "1", &spec(&exe, &start_dir), &Artwork::default()).expect("second sync");
        assert_eq!(appid_1, appid_2);

        let path = paths::shortcuts_vdf_path(&steam_root, "1");
        let bytes = fs::read(&path).expect("shortcuts.vdf written");
        let (_, root) = binary_vdf::decode(&bytes).expect("valid vdf");
        let Value::Map(entries) = root else {
            panic!("root is a map")
        };
        assert_eq!(entries.len(), 1, "second sync must replace, not append");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_deletes_only_the_matching_entry() {
        let dir = std::env::temp_dir().join(format!("decanter-steam-export-test-remove-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let steam_root = dir.join("Steam");
        let start_dir = dir.clone();

        let exe_a = dir.join("a.exe");
        let exe_b = dir.join("b.exe");
        let appid_a = sync(&steam_root, "1", &spec(&exe_a, &start_dir), &Artwork::default()).expect("sync a");
        let appid_b = sync(&steam_root, "1", &spec(&exe_b, &start_dir), &Artwork::default()).expect("sync b");
        assert_ne!(appid_a, appid_b);

        remove(&steam_root, "1", appid_a).expect("remove a");

        let path = paths::shortcuts_vdf_path(&steam_root, "1");
        let bytes = fs::read(&path).expect("shortcuts.vdf written");
        let (_, root) = binary_vdf::decode(&bytes).expect("valid vdf");
        let Value::Map(entries) = root else {
            panic!("root is a map")
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.get("appid").and_then(Value::as_int), Some(appid_b));
        assert_eq!(entries[0].0, "0", "remaining entry must be renumbered to 0");

        let _ = fs::remove_dir_all(&dir);
    }
}
