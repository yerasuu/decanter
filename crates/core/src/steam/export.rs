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
use super::process::SteamState;
use super::{paths, process};

/// What a shortcut starts. Steam runs `exe` from `start_dir` with `launch_options` after it, the
/// whole line through `/bin/sh -c` on Linux, so the options must hold nothing a shell expands.
pub struct Command {
    pub exe: PathBuf,
    pub start_dir: PathBuf,
    pub launch_options: String,
}

/// What a shortcut entry needs beyond its artwork.
pub struct ShortcutSpec<'spec> {
    pub name: &'spec str,
    pub command: &'spec Command,
}

/// One entry decanter wrote, as `shortcuts.vdf` (or the staged change) holds it.
pub struct Own {
    pub appid: i32,
    /// The `Exe` field as written, quotes included.
    pub exe: String,
    pub launch_options: String,
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
pub(super) const TAG: &str = "Decanter";

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
    let exe_field = exe_field(spec.command);
    let start_dir_field = quote(&spec.command.start_dir.to_string_lossy());
    let appid = legacy_appid(&exe_field, spec.name);
    let grid_id = grid_appid(appid);

    let icon_dest = place_grid_image(steam_root, user_id, grid_id, "_icon", artwork.icon.as_deref())?;
    place_grid_image(steam_root, user_id, grid_id, "", artwork.capsule.as_deref())?;
    place_grid_image(steam_root, user_id, grid_id, "p", artwork.portrait.as_deref())?;
    place_grid_image(steam_root, user_id, grid_id, "_hero", artwork.hero.as_deref())?;

    let icon = icon_dest.map_or_else(String::new, |path| path.to_string_lossy().into_owned());
    let entry = Value::map([
        ("appid", appid.into()),
        ("AppName", spec.name.into()),
        ("Exe", exe_field.into()),
        ("StartDir", start_dir_field.into()),
        ("icon", icon.into()),
        ("ShortcutPath", "".into()),
        ("LaunchOptions", spec.command.launch_options.as_str().into()),
        ("IsHidden", 0.into()),
        ("AllowDesktopConfig", 1.into()),
        ("AllowOverlay", 1.into()),
        ("OpenVR", 0.into()),
        ("Devkit", 0.into()),
        ("DevkitGameID", "".into()),
        ("DevkitOverrideAppID", 0.into()),
        ("LastPlayTime", 0.into()),
        ("FlatpakAppID", "".into()),
        ("tags", Value::map([("0", TAG.into())])),
    ]);

    let path = resolve_write_target(steam_root, user_id)?;
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
    let path = resolve_write_target(steam_root, user_id)?;
    let mut root = read_shortcuts(&path)?;
    if let Value::Map(entries) = &mut root {
        entries.retain(|(_, entry)| entry.get("appid").and_then(Value::as_int) != Some(appid));
        renumber(entries);
    }
    write_shortcuts(&path, &root)?;
    remove_grid_art(steam_root, user_id, appid);
    Ok(())
}

/// The appid of the entry decanter wrote whose launch options `matches`, read from the staged
/// change when one waits, since that is what Steam reads next.
///
/// # Errors
/// Fails if the file read exists but isn't valid binary VDF, or can't be read.
#[inline]
pub fn find(steam_root: &Path, user_id: &str, matches: impl Fn(&str) -> bool) -> io::Result<Option<i32>> {
    Ok(own_entries(steam_root, user_id)?
        .into_iter()
        .find(|own| matches(&own.launch_options))
        .map(|own| own.appid))
}

/// Every entry decanter wrote, from the staged change when one waits, since that is what Steam
/// reads next.
///
/// # Errors
/// Fails if the file read exists but isn't valid binary VDF, or can't be read.
#[inline]
pub fn own_entries(steam_root: &Path, user_id: &str) -> io::Result<Vec<Own>> {
    let pending = pending_path(steam_root, user_id);
    let path = if pending.is_file() {
        pending
    } else {
        paths::shortcuts_vdf_path(steam_root, user_id)
    };
    let root = read_shortcuts(&path)?;
    Ok(root
        .as_map()
        .into_iter()
        .flatten()
        .filter(|(_, entry)| is_own(entry))
        .filter_map(|(_, entry)| {
            Some(Own {
                appid: entry.get("appid")?.as_int()?,
                exe: entry.get("Exe")?.as_str()?.to_owned(),
                launch_options: entry.get("LaunchOptions")?.as_str()?.to_owned(),
            })
        })
        .collect())
}

/// Points decanter's entry `appid` at `command`, keeping its name, art and whatever Steam or the
/// user changed on it. The appid hashes `Exe` and the name, so the entry and its grid art move to
/// the new one; when an entry with that appid is already there, the old one only goes. `None` when
/// there is no entry of decanter's under `appid`.
///
/// # Errors
/// Fails if `shortcuts.vdf` exists but isn't valid binary VDF, a grid file can't be moved, or the
/// rewritten file can't be written.
#[inline]
pub fn retarget(steam_root: &Path, user_id: &str, appid: i32, command: &Command) -> io::Result<Option<i32>> {
    let path = resolve_write_target(steam_root, user_id)?;
    let mut root = read_shortcuts(&path)?;
    let Value::Map(entries) = &mut root else {
        return Ok(None);
    };
    let Some(index) = entries
        .iter()
        .position(|(_, entry)| is_own(entry) && entry.get("appid").and_then(Value::as_int) == Some(appid))
    else {
        return Ok(None);
    };
    let exe = exe_field(command);
    let name = entries[index]
        .1
        .get("AppName")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let new_appid = legacy_appid(&exe, name);
    let is_taken = new_appid != appid
        && entries
            .iter()
            .any(|(_, entry)| entry.get("appid").and_then(Value::as_int) == Some(new_appid));
    if is_taken {
        entries.remove(index);
        renumber(entries);
        write_shortcuts(&path, &root)?;
        remove_grid_art(steam_root, user_id, appid);
        return Ok(Some(new_appid));
    }
    let icon = move_grid_art(steam_root, user_id, appid, new_appid)?;
    if let Value::Map(fields) = &mut entries[index].1 {
        let start_dir = quote(&command.start_dir.to_string_lossy());
        set(fields, "appid", new_appid.into());
        set(fields, "Exe", exe.into());
        set(fields, "StartDir", start_dir.into());
        set(fields, "LaunchOptions", command.launch_options.as_str().into());
        if let Some(icon) = icon {
            set(fields, "icon", icon.to_string_lossy().into_owned().into());
        }
    }
    write_shortcuts(&path, &root)?;
    Ok(Some(new_appid))
}

fn is_own(entry: &Value) -> bool {
    entry
        .get("tags")
        .and_then(Value::as_map)
        .is_some_and(|tags| tags.iter().any(|(_, tag)| tag.as_str() == Some(TAG)))
}

fn set(fields: &mut Vec<(String, Value)>, key: &str, value: Value) {
    match fields.iter_mut().find(|(existing, _)| existing == key) {
        Some(slot) => slot.1 = value,
        None => fields.push((key.to_owned(), value)),
    }
}

/// The `Exe` field for `command`: the hash input of the appid, quotes included.
fn exe_field(command: &Command) -> String {
    quote(&command.exe.to_string_lossy())
}

/// Where a staged change waits when Steam is running and can't be written
/// to directly — see [`resolve_write_target`].
fn pending_path(steam_root: &Path, user_id: &str) -> PathBuf {
    paths::shortcuts_vdf_path(steam_root, user_id).with_extension("vdf.pending")
}

/// Whether a staged change is waiting for [`apply_pending_and_restart_steam`]
/// to pick it up.
#[must_use]
#[inline]
pub fn has_pending_changes(steam_root: &Path, user_id: &str) -> bool {
    pending_path(steam_root, user_id).is_file()
}

/// Picks the file [`sync`]/[`remove`] should actually write to: the real
/// `shortcuts.vdf`, unless Steam is running or can't be ruled out — in
/// which case any direct write would just get clobbered the next time
/// Steam re-saves its own in-memory copy of that file (confirmed
/// behavior, not a guess: an entry written this way was later found
/// wiped, with a live Steam client running the whole time). Instead the change goes to a pending file
/// beside it, seeded from whatever the real file currently holds the
/// first time one is needed, so it starts as a faithful copy rather than
/// dropping entries Steam already has that decanter doesn't know about.
fn resolve_write_target(steam_root: &Path, user_id: &str) -> io::Result<PathBuf> {
    let real = paths::shortcuts_vdf_path(steam_root, user_id);
    if process::state(steam_root) == SteamState::NotRunning {
        return Ok(real);
    }
    let pending = pending_path(steam_root, user_id);
    if !pending.is_file() {
        let seed = read_shortcuts(&real)?;
        write_shortcuts(&pending, &seed)?;
    }
    Ok(pending)
}

/// Applies whatever is staged in the pending file to the real
/// `shortcuts.vdf`, restarting Steam around the swap so it reads the
/// result fresh instead of clobbering it again. A no-op if nothing is
/// pending.
///
/// Steam is asked to shut down cleanly (`steam -shutdown`) rather than
/// killed outright, and this call blocks until it's confirmed gone before
/// touching the file — writing while it's still tearing down would be the
/// same race this exists to avoid.
///
/// `user_confirmed_closed` is the user's word that Steam is closed, the
/// only answer left when [`process::state`] can't tell.
///
/// # Errors
/// [`ApplyError::SteamStateUnknown`] when whether Steam runs can't be
/// told from here and the user hasn't confirmed it closed; nothing is
/// touched then. Otherwise fails if Steam doesn't exit within the
/// shutdown timeout (nothing is overwritten in that case), if the pending
/// file can't be moved into place, or if Steam can't be relaunched
/// afterward.
#[inline]
pub fn apply_pending_and_restart_steam(
    steam_root: &Path,
    user_id: &str,
    user_confirmed_closed: bool,
) -> Result<(), ApplyError> {
    let pending = pending_path(steam_root, user_id);
    if !pending.is_file() {
        return Ok(());
    }

    match process::state(steam_root) {
        SteamState::Running => {
            process::shutdown(steam_root)?;
            process::wait_for_exit(steam_root)?;
        }
        SteamState::NotRunning => {}
        SteamState::Unknown if user_confirmed_closed => {}
        // Asking for a shutdown wouldn't help: wait_for_exit could never
        // confirm Steam gone, since the same probe would still say Unknown.
        SteamState::Unknown => return Err(ApplyError::SteamStateUnknown),
    }

    let real = paths::shortcuts_vdf_path(steam_root, user_id);
    // Steam goes back up even when the swap failed, since this shut it down.
    let applied = read_shortcuts(&real)
        .and_then(|saved| Ok((saved, read_shortcuts(&pending)?)))
        .and_then(|(saved, staged)| write_shortcuts(&real, &merge(saved, staged)))
        .and_then(|()| fs::remove_file(&pending));
    let launched = process::launch(steam_root);
    applied?;
    Ok(launched?)
}

/// The entries Steam saved on its way out that aren't decanter's, then decanter's as staged: the
/// staged copy was seeded from the file as it was when the first change was staged, and since then
/// Steam may have saved a shortcut the user added or removed in it, which the staged copy would
/// otherwise put back as it was.
fn merge(saved: Value, staged: Value) -> Value {
    let (saved, staged) = match (saved, staged) {
        (Value::Map(saved), Value::Map(staged)) => (saved, staged),
        (_, staged @ Value::Map(_)) => return staged,
        (saved, _) => return saved,
    };
    let mut entries: Vec<_> = saved
        .into_iter()
        .filter(|(_, entry)| !is_own(entry))
        .chain(staged.into_iter().filter(|(_, entry)| is_own(entry)))
        .collect();
    renumber(&mut entries);
    Value::Map(entries)
}

/// Why a staged change couldn't be applied.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("cannot tell whether steam is running")]
    SteamStateUnknown,
    #[error(transparent)]
    Io(#[from] io::Error),
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
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Value::Map(Vec::new())),
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

/// The grid files Steam looks a shortcut's art up by, after its grid appid.
const GRID_SUFFIXES: [&str; 4] = ["", "p", "_hero", "_icon"];

fn remove_grid_art(steam_root: &Path, user_id: &str, appid: i32) {
    let grid_id = grid_appid(appid);
    let dir = paths::grid_dir(steam_root, user_id);
    for suffix in GRID_SUFFIXES {
        remove_any_extension(&dir, &format!("{grid_id}{suffix}"));
    }
}

/// Renames the art of shortcut `from` to that of `to`, returning where its icon went, if it had one.
fn move_grid_art(steam_root: &Path, user_id: &str, from: i32, to: i32) -> io::Result<Option<PathBuf>> {
    let dir = paths::grid_dir(steam_root, user_id);
    let (from, to) = (grid_appid(from), grid_appid(to));
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut icon = None;
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        for suffix in GRID_SUFFIXES {
            let Some(ext) = name
                .strip_prefix(&format!("{from}{suffix}"))
                .filter(|rest| rest.starts_with('.'))
            else {
                continue;
            };
            let dest = dir.join(format!("{to}{suffix}{ext}"));
            fs::rename(entry.path(), &dest)?;
            if suffix == "_icon" {
                icon = Some(dest);
            }
            break;
        }
    }
    Ok(icon)
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
