//! A title's own launch settings, `titles/<TITLE_ID>/settings.json` beside its port ini. Every key
//! is the title's except [`SHARED`], read from the shared file whatever the title's says: an
//! allow-list, so that a key added later belongs to each game unless it is named there. A title's
//! file starts as what that title ran with before, so nothing changes for the player that day; the
//! shared file is left as it was, what an older port's launches read and what a title seen for the
//! first time starts from.

use core::iter;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{LauncherSettings, TITLES, path};
use crate::bbport::{Names, direct, frame_gen, local_player, online, port_ini};
use crate::os::windows::package::{PINNED_KEY, SELECTED_KEY};
use crate::paths::Root;
use crate::port::Port;
use crate::title_id::TitleId;

const FILE: &str = "settings.json";

/// The exceptions of `docs/ps-per-game-settings.md` (E1-E3), and what later phases move.
const SHARED: [&str; 11] = [
    // The shadNet account is who the user is, not how a game runs; whether a game plays online
    // is the game's.
    online::NPID,
    online::SERVER,
    online::WEBAPI,
    online::COMMUNITY,
    // Folders of the user's console dump, which have no per-game value.
    "sys_modules_dir",
    "sys_fonts_dir",
    // What runs the games, not how one runs.
    direct::FOLDER_KEY,
    SELECTED_KEY,
    PINNED_KEY,
    // Shared until mods and patches are per game.
    "mods_dir",
    "patches_dir",
];

/// What a title's file of an earlier format lacks: 1 (no field) is from before [`MOVED`].
const FORMAT: u64 = 2;
const FORMAT_KEY: &str = "format";

/// The keys every title shared before [`FORMAT`] 2: a title's file of format 1 takes the shared
/// values once, so the game keeps playing as it did.
const MOVED: [&str; 4] = [online::ONLINE, online::UPNP, online::BBSC, online::BBSC_LOCAL_SERVER];

/// Keys of bbport's former GTK launcher that nothing reads: left in the shared file, never copied
/// into a title's.
const UNREAD: [&str; 4] = ["ui_language", "game_dir", "user_dir", "mangohud"];

/// Keys that only ever meant one title, under `titles.<TITLE_ID>`: a top-level one in the shared
/// file is not that title's value, so it is not copied.
const TITLE_ONLY: [&str; 3] = [frame_gen::KEY, local_player::KEY, "debug"];

impl LauncherSettings {
    /// Whether every title reads `key` from the shared file, which [`Self::edit`] writes, rather
    /// than from its own.
    #[must_use]
    pub fn is_shared(key: &str) -> bool {
        SHARED.contains(&key)
    }

    /// What `title_id` launches with and its tabs show: its own file, with the shared keys of the
    /// shared one. Where the title has no file of its own (an older port, an unusual id) it is the
    /// shared file, as before. File I/O: off the UI thread.
    ///
    /// # Errors
    /// Making the title's file the first time (backing up the shared one, reading it, writing the
    /// copy), or a title's file that can't be read or doesn't parse.
    pub fn of_title(root: &Root, title_id: &str) -> io::Result<Self> {
        let shared = Self::load(root);
        let Some(path) = title_file(root, title_id)? else {
            return Ok(shared);
        };
        let mut own = Self::read_at(&path, true)?;
        own.values.retain(|key, _| !Self::is_shared(key));
        own.values
            .extend(shared.values.into_iter().filter(|(key, _)| Self::is_shared(key)));
        Ok(own)
    }

    /// One change of `title_id`'s own keys against its file as it is now; the shared file where
    /// the title has none of its own. A shared key set here is dropped, since the title's file is
    /// never read for it. Fails rather than overwrite a file it can't read or parse.
    ///
    /// # Errors
    /// As [`Self::of_title`], and writing the file.
    pub fn edit_title(root: &Root, title_id: &str, change: impl FnOnce(&mut Self)) -> io::Result<()> {
        let Some(path) = title_file(root, title_id)? else {
            return Self::edit(root, change);
        };
        let mut own = Self::read_at(&path, true)?;
        change(&mut own);
        own.values.retain(|key, _| !Self::is_shared(key));
        own.write_at(root, &path)
    }

    /// `key`'s value in the shared file and in every title's own, for what must know them all,
    /// such as the pad names a report hides. A file that can't be read is skipped and logged.
    /// File I/O: off the UI thread.
    pub fn every_text<'key>(root: &Root, key: &'key str) -> impl Iterator<Item = String> + 'key {
        let titles = fs::read_dir(root.titles())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.parse::<TitleId>().is_ok())
            })
            .map(|entry| entry.path().join(FILE))
            .filter(|path| path.is_file());
        iter::once(path(root))
            .chain(titles)
            .filter_map(move |path| match Self::read_at(&path, false) {
                Ok(settings) => settings.text(key).map(str::to_owned),
                Err(err) => {
                    tracing::warn!(path = %path.display(), %err, "launch settings skipped");
                    None
                }
            })
    }
}

/// `title_id`'s own file, made the first time it is asked for; `None` where every title reads the
/// shared one.
fn title_file(root: &Root, title_id: &str) -> io::Result<Option<PathBuf>> {
    let names = Names::of_blocking(&Port::Bbport.checkout(root));
    let Ok(id) = port_ini::own_title(names, title_id) else {
        return Ok(None);
    };
    let path = root.title(&id).join(FILE);
    if path.exists() {
        catch_up(root, &id, &path)?;
    } else {
        create(root, &id, &path)?;
    }
    Ok(Some(path))
}

/// A file made before the [`MOVED`] keys were each title's takes the shared values it lacks, once;
/// the shared file was backed up when the first title took its copy, and is not written.
fn catch_up(root: &Root, id: &TitleId, path: &Path) -> io::Result<()> {
    let mut own = LauncherSettings::read_at(path, true)?;
    if own.values.get(FORMAT_KEY).and_then(Value::as_u64) >= Some(FORMAT) {
        return Ok(());
    }
    let mut shared = LauncherSettings::read(root)?;
    let mut copied = Vec::with_capacity(MOVED.len());
    for key in MOVED {
        if !own.values.contains_key(key)
            && let Some(value) = shared.values.remove(key)
        {
            own.values.insert(key.into(), value);
            copied.push(key);
        }
    }
    own.values.insert(FORMAT_KEY.into(), FORMAT.into());
    own.write_at(root, path)?;
    tracing::info!(title_id = id.as_str(), keys = ?copied, to = %path.display(), "title launch settings caught up with the online switches");
    Ok(())
}

/// What `id` ran with before it had a file: the shared file's keys but those every title still
/// shares and those nothing reads, its own `titles.<TITLE_ID>` entries on top (so a switch on for
/// every title stays on). The shared file is backed up first, before the first title takes a
/// copy, and fails the copy when it doesn't parse rather than give the title the port's defaults.
fn create(root: &Root, id: &TitleId, path: &Path) -> io::Result<()> {
    if !any_title_has_file(root) {
        port_ini::back_up_shared(root, &[FILE])?;
    }
    let mut shared = LauncherSettings::read(root)?;
    let own = match shared.values.remove(TITLES) {
        Some(Value::Object(mut titles)) => titles.remove(id.as_str()),
        _ => None,
    };
    let mut values: serde_json::Map<String, Value> = shared
        .values
        .into_iter()
        .filter(|(key, _)| {
            let name = key.as_str();
            !LauncherSettings::is_shared(name) && !UNREAD.contains(&name) && !TITLE_ONLY.contains(&name)
        })
        .collect();
    if let Some(Value::Object(entries)) = own {
        values.extend(entries);
    }
    let copied = values.len();
    values.insert("created_from".into(), "migration".into());
    values.insert("created_on".into(), port_ini::today().into());
    values.insert(FORMAT_KEY.into(), FORMAT.into());
    let settings = LauncherSettings { values, is_title: true };
    match settings.write_at(root, path) {
        Ok(()) => {
            tracing::info!(title_id = id.as_str(), keys = copied, to = %path.display(), "title launch settings created");
            Ok(())
        }
        // Another thread made the same copy at the same moment and renamed it first.
        Err(_) if path.is_file() => Ok(()),
        Err(err) => Err(err),
    }
}

fn any_title_has_file(root: &Root) -> bool {
    fs::read_dir(root.titles())
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.path().join(FILE).is_file())
}
