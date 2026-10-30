//! Which catalog layout each game took, as that game's own record: `pad_layout` and `key_layout` in
//! a bbport title's `settings.json` (so they are copied, reset and exported with its Gamepad and
//! Keyboard tabs), and `layout.json` beside an AnyPS5 title's `anyps5-input.ini`. What the game's
//! lines hold beyond that layout are its own changes. A title whose settings are the shared ones (an
//! older port, an unusual id) records nothing, since its lines are every title's: its layout is the
//! one they equal.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{KeyLayout, Kind};
use crate::anyps5::AnyPs5;
use crate::bbport::pad::{self, PadLayout};
use crate::bbport::{LauncherSettings, Names};
use crate::mapping::Target;
use crate::paths::Root;
use crate::title_id::TitleId;

pub const PAD_KEY: &str = "pad_layout";
pub const KEYS_KEY: &str = "key_layout";
const ANYPS5_FILE: &str = "layout.json";
const SETTINGS: &str = "settings.json";

#[derive(Default, Serialize, Deserialize)]
struct AnyPs5Choice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keys: Option<String>,
}

const fn key(kind: Kind) -> &'static str {
    match kind {
        Kind::Pad => PAD_KEY,
        Kind::Keys(_) => KEYS_KEY,
    }
}

fn anyps5_file(root: &Root, id: &TitleId) -> PathBuf {
    AnyPs5::output_dir(root, id.as_str()).join(ANYPS5_FILE)
}

fn read_anyps5(path: &Path) -> io::Result<Option<String>> {
    match fs::read(path) {
        Ok(data) => serde_json::from_slice::<AnyPs5Choice>(decanter_core::bom::strip(&data, path))
            .map(|choice| choice.keys)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {err}", path.display()))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// The layout `title_id` recorded, `None` when it recorded none. File I/O: off the UI thread.
///
/// # Errors
/// The title's file that can't be read or doesn't parse.
pub fn recorded(root: &Root, kind: Kind, title_id: &str) -> io::Result<Option<String>> {
    if kind == Kind::Keys(Target::AnyPs5) {
        return title_id
            .parse::<TitleId>()
            .map_or(Ok(None), |id| read_anyps5(&anyps5_file(root, &id)));
    }
    LauncherSettings::own_text(root, title_id, key(kind))
}

/// Records that `title_id` took `layout_id`; nothing where the title's settings are the shared ones.
/// File I/O: off the UI thread.
///
/// # Errors
/// Reading or writing the title's file.
pub fn record(root: &Root, kind: Kind, title_id: &str, layout_id: &str) -> io::Result<()> {
    if kind != Kind::Keys(Target::AnyPs5) {
        return LauncherSettings::set_own_text(root, title_id, key(kind), layout_id).map(drop);
    }
    let Ok(id) = title_id.parse::<TitleId>() else {
        return Ok(());
    };
    let text = serde_json::to_string_pretty(&AnyPs5Choice {
        keys: Some(layout_id.to_owned()),
    })
    .map_err(io::Error::other)?;
    crate::files::write_atomic(&anyps5_file(root, &id), text.as_bytes())
}

/// Records `layout_id` in the defaults for new games, which copy it with the lines it came with;
/// nothing for AnyPS5, whose titles have no defaults. File I/O: off the UI thread.
///
/// # Errors
/// Making, reading or writing the defaults.
pub fn record_default(root: &Root, kind: Kind, layout_id: &str) -> io::Result<()> {
    if kind == Kind::Keys(Target::AnyPs5) {
        return Ok(());
    }
    LauncherSettings::edit_defaults(root, |defaults| defaults.set_text(key(kind), layout_id))
}

fn titles_in(dir: PathBuf) -> impl Iterator<Item = TitleId> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<TitleId>().ok())
}

/// The titles that use `layout`: those that recorded it, and those that recorded none and whose
/// lines equal it. A title whose file can't be read is skipped and logged. File I/O: off the UI
/// thread.
#[must_use]
pub fn pad_users(root: &Root, layout: &PadLayout) -> Vec<String> {
    users(root, Kind::Pad, &layout.id, |path| {
        pad::current(path, [layout]).is_some()
    })
}

/// As [`pad_users`], for a keyboard layout of `target`.
#[must_use]
pub fn key_users(root: &Root, target: Target, layout: &KeyLayout) -> Vec<String> {
    users(root, Kind::Keys(target), &layout.id, |path| {
        crate::binds::read(target, path).is_ok_and(|mapping| mapping == layout.mapping(target))
    })
}

fn users(root: &Root, kind: Kind, id: &str, matches: impl Fn(&Path) -> bool) -> Vec<String> {
    let is_anyps5 = kind == Kind::Keys(Target::AnyPs5);
    titles_in(if is_anyps5 { root.anyps5() } else { root.titles() })
        .filter(|title| {
            let (found, lines) = if is_anyps5 {
                (
                    read_anyps5(&anyps5_file(root, title)),
                    AnyPs5::binds_path(root, title.as_str()),
                )
            } else {
                (
                    LauncherSettings::text_at(&root.title(title).join(SETTINGS), key(kind)),
                    root.title(title).join(Names::Psport.ini()),
                )
            };
            match found {
                Ok(Some(recorded)) => recorded == id,
                Ok(None) => lines.is_file() && matches(&lines),
                Err(err) => {
                    tracing::warn!(title_id = title.as_str(), %err, "layout choice skipped");
                    false
                }
            }
        })
        .map(|title| title.as_str().to_owned())
        .collect()
}

/// Drops `layout_id` from every game that recorded it, and from the defaults for new games.
pub(super) fn forget(root: &Root, kind: Kind, layout_id: &str) -> io::Result<usize> {
    let mut first_error = None;
    let mut forgotten = 0_usize;
    let mut note = |result: io::Result<bool>, path: &Path| match result {
        Ok(true) => forgotten = forgotten.saturating_add(1),
        Ok(false) => {}
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "deleted layout not dropped from a game's record");
            first_error.get_or_insert(err);
        }
    };
    if kind == Kind::Keys(Target::AnyPs5) {
        for title in titles_in(root.anyps5()) {
            let path = anyps5_file(root, &title);
            note(forget_anyps5(&path, layout_id), &path);
        }
    } else {
        let defaults = root.defaults().join(SETTINGS);
        for path in titles_in(root.titles())
            .map(|title| root.title(&title).join(SETTINGS))
            .chain([defaults])
        {
            note(
                LauncherSettings::forget_text_at(root, &path, key(kind), layout_id),
                &path,
            );
        }
    }
    tracing::info!(
        layout = layout_id,
        games = forgotten,
        "deleted layout dropped from the games that recorded it"
    );
    first_error.map_or(Ok(forgotten), Err)
}

fn forget_anyps5(path: &Path, layout_id: &str) -> io::Result<bool> {
    if read_anyps5(path)?.as_deref() != Some(layout_id) {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}
