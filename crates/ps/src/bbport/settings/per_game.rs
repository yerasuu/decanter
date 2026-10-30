//! A game's own settings taken as a whole, or by [`Category`]: copied from another game or into the
//! defaults, reset to the defaults, exported and imported ([`super::transfer`]), and read or changed
//! one key at a time from the command line. What every title shares (the account, the dump folders,
//! the port in use) is never touched. Each replacement backs the files it overwrites up into
//! `backups/per-game-settings-<date>/` first, since it removes what the user chose (#1133).

use std::fs;
use std::io;
use std::path::PathBuf;

use serde_json::Value;

use super::keys::{self, ini_category, launch_category};
use super::{LauncherSettings, defaults, title};
use crate::bbport::tools::{MODS_FILE, ModProfile, PATCHES_FILE, PatchProfile};
use crate::bbport::{Bbport, Ini, Names, Store, mods, port_ini};
use crate::paths::Root;
use crate::port::Port;
use crate::title_id::TitleId;

pub use super::keys::{Category, Refusal};

/// What names the defaults where a title id is expected.
pub const DEFAULTS: &str = "defaults";

#[derive(Debug, thiserror::Error)]
pub enum PerGameError {
    #[error("the port in use predates per-game settings")]
    OlderPort,
    #[error("{0} has no settings of its own yet")]
    NoSettings(String),
    #[error("the source and the destination are the same")]
    SameTarget,
    #[error("the file holds the settings of {file}, not of {target}")]
    OtherTitle { file: String, target: String },
    #[error(transparent)]
    Refused(#[from] Refusal),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Whose settings: a title's own, or the defaults new titles copy.
pub enum Target {
    Title(TitleId),
    Defaults,
}

impl Target {
    /// [`DEFAULTS`], or a title id, checked before it names a folder.
    ///
    /// # Errors
    /// Not a title id, or a checkout whose port reads one shared ini for every title.
    pub fn parse(root: &Root, text: &str) -> Result<Self, PerGameError> {
        if text == DEFAULTS {
            return Ok(Self::Defaults);
        }
        let id = text
            .parse()
            .map_err(|_| Refusal::TitleId(text.escape_default().to_string()))?;
        if !matches!(Names::of_blocking(&Port::Bbport.checkout(root)), Names::Psport) {
            return Err(PerGameError::OlderPort);
        }
        Ok(Self::Title(id))
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Title(id) => id.as_str(),
            Self::Defaults => DEFAULTS,
        }
    }
}

/// One of a game's settings as [`list`] shows it.
pub struct Entry {
    pub category: Category,
    pub store: Store,
    pub key: String,
    pub value: String,
}

/// A target's files. The defaults hold no mod or patch choices: those depend on the mods a game's
/// folder holds.
pub(super) struct Files {
    pub(super) settings: PathBuf,
    pub(super) ini: PathBuf,
    pub(super) choices: Option<(PathBuf, PathBuf)>,
}

/// `target`'s files, made from the defaults where `create` allows it; without, a title whose folder
/// isn't there yet has none to read.
pub(super) fn files(root: &Root, target: &Target, create: bool) -> Result<Files, PerGameError> {
    match target {
        Target::Defaults => {
            LauncherSettings::defaults(root)?;
            Ok(Files {
                settings: defaults::path(root),
                ini: Bbport::defaults_ini(root)?,
                choices: None,
            })
        }
        Target::Title(id) => {
            if !create && !root.title(id).is_dir() {
                return Err(PerGameError::NoSettings(id.as_str().to_owned()));
            }
            let settings = title::title_file(root, id.as_str())?.ok_or(PerGameError::OlderPort)?;
            let ini = Bbport::title_ini(root, id.as_str())?.path;
            let mods = port_ini::title_copy(root, id, MODS_FILE)?;
            let patches = port_ini::title_copy(root, id, PATCHES_FILE)?;
            Ok(Files {
                settings,
                ini,
                choices: Some((mods, patches)),
            })
        }
    }
}

/// What replaces a target's settings: launch keys, port ini lines and mod and patch choices.
pub(super) struct Content {
    pub(super) launch: serde_json::Map<String, Value>,
    pub(super) ini: Vec<(String, String)>,
    pub(super) choices: Option<(ModProfile, PatchProfile)>,
}

impl Content {
    pub(super) fn read(files: &Files) -> io::Result<Self> {
        let launch = LauncherSettings::read_at(&files.settings, true)?.values;
        let ini = Ini::load(&files.ini);
        let mut lines: Vec<(String, String)> = ini
            .prefixed("")
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        lines.sort();
        Ok(Self {
            launch,
            ini: lines,
            choices: files
                .choices
                .as_ref()
                .map(|(mods, patches)| (ModProfile::load(mods), PatchProfile::load(patches))),
        })
    }
}

/// Backs up `target`'s files into `backups/per-game-settings-<date>/<target>-<time>/`, a folder of
/// its own each time, so a second change the same day never replaces the first copy.
pub(super) fn back_up(root: &Root, target: &Target, files: &Files) -> io::Result<PathBuf> {
    let day = root.backups().join(format!("per-game-settings-{}", port_ini::today()));
    let name = format!("{}-{}", target.name(), port_ini::now_hms());
    let mut dir = day.join(&name);
    let mut attempt = 1_u32;
    while dir.exists() {
        attempt = attempt.saturating_add(1);
        dir = day.join(format!("{name}-{attempt}"));
    }
    fs::create_dir_all(&dir)?;
    let mut copied = 0_usize;
    let choices = files.choices.iter().flat_map(|(mods, patches)| [mods, patches]);
    for path in [&files.settings, &files.ini].into_iter().chain(choices) {
        if let Some(file) = path.file_name().filter(|_| path.is_file()) {
            if path == &files.settings {
                super::copy_without_secrets(path, &dir.join(file))?;
            } else {
                fs::copy(path, dir.join(file))?;
            }
            copied = copied.saturating_add(1);
        }
    }
    tracing::info!(target = target.name(), dir = %dir.display(), copied, "game settings backed up before they are replaced");
    Ok(dir)
}

/// Replaces the `categories` of `target`'s settings with those of `content`, after a backup, and
/// returns the backup's folder. A category's keys `content` lacks go, so the target ends as the
/// source is; the rest of the target's settings stay. `origin` is recorded in a title's file when
/// the whole of it was replaced.
pub(super) fn replace(
    root: &Root,
    target: &Target,
    content: Content,
    categories: &[Category],
    origin: Option<String>,
) -> Result<PathBuf, PerGameError> {
    let files = files(root, target, true)?;
    let backup = back_up(root, target, &files)?;
    let is_defaults = matches!(target, Target::Defaults);
    // A new title's mods and patches folders are its own, never those of a game the defaults came
    // from.
    let carries = |key: &str| {
        launch_category(key).is_some_and(|category| categories.contains(&category))
            && !(is_defaults && mods::FOLDER_KEYS.contains(&key))
    };
    let mut own = LauncherSettings::read_at(&files.settings, true)?;
    own.values.retain(|key, _| !carries(key));
    own.values
        .extend(content.launch.into_iter().filter(|(key, _)| carries(key)));
    if let Some(origin) = origin.filter(|_| !is_defaults) {
        title::record_origin(&mut own.values, origin);
    }
    own.write_at(root, &files.settings)?;
    let carries_line = |key: &str| ini_category(key).is_some_and(|category| categories.contains(&category));
    let mut ini = Ini::load(&files.ini);
    let stale: Vec<String> = ini
        .prefixed("")
        .filter(|(key, _)| carries_line(key))
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in &stale {
        ini.remove(key);
    }
    for (key, value) in content.ini.iter().filter(|(key, _)| carries_line(key)) {
        ini.set(key, value);
    }
    ini.save(&files.ini)?;
    if categories.contains(&Category::Mods)
        && let (Some((mods_path, patches_path)), Some((mods, patches))) = (&files.choices, content.choices)
    {
        mods.save(mods_path)?;
        patches.save(patches_path)?;
    }
    tracing::info!(
        target = target.name(),
        categories = ?categories.iter().map(|category| category.id()).collect::<Vec<_>>(),
        "game settings replaced"
    );
    Ok(backup)
}

/// Copies `categories` of `from`'s settings over `to`'s, after a backup of `to`'s; returns the
/// backup's folder. Copied, never linked: a later change of either reaches only that one. File I/O:
/// off the UI thread.
///
/// # Errors
/// The same title twice, a source with no settings yet, or a file that can't be read or written.
pub fn copy(root: &Root, from: &Target, to: &Target, categories: &[Category]) -> Result<PathBuf, PerGameError> {
    if from.name() == to.name() {
        return Err(PerGameError::SameTarget);
    }
    let content = Content::read(&files(root, from, false)?)?;
    let origin = (categories.len() == Category::ALL.len()).then(|| format!("copy:{}", from.name()));
    replace(root, to, content, categories, origin)
}

/// `title`'s settings as a title seen for the first time gets them, after a backup; its mods and
/// patches stay, since the defaults hold no mod choices. Returns the backup's folder. File I/O: off
/// the UI thread.
///
/// # Errors
/// The defaults given as `title`, or a file that can't be read or written.
pub fn reset(root: &Root, title: &Target) -> Result<PathBuf, PerGameError> {
    if matches!(title, Target::Defaults) {
        return Err(PerGameError::SameTarget);
    }
    let content = Content::read(&files(root, &Target::Defaults, true)?)?;
    let categories: Vec<Category> = Category::ALL
        .into_iter()
        .filter(|&category| category != Category::Mods)
        .collect();
    replace(root, title, content, &categories, Some(DEFAULTS.to_owned()))
}

/// The titles with settings of their own, by title id. File I/O: off the UI thread.
#[must_use]
pub fn titles(root: &Root) -> Vec<String> {
    let mut titles: Vec<String> = fs::read_dir(root.titles())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.parse::<TitleId>().is_ok())
        .collect();
    titles.sort();
    titles
}

/// Every setting `target` sets, by category, file and key. File I/O: off the UI thread.
///
/// # Errors
/// A title with no settings yet, or a file that can't be read.
pub fn list(root: &Root, target: &Target) -> Result<Vec<Entry>, PerGameError> {
    let content = Content::read(&files(root, target, false)?)?;
    let launch = content.launch.iter().filter_map(|(key, value)| {
        launch_category(key).map(|category| Entry {
            category,
            store: Store::Launcher,
            key: key.clone(),
            value: shown(key, value),
        })
    });
    let ini = content.ini.into_iter().filter_map(|(key, value)| {
        ini_category(&key).map(|category| Entry {
            category,
            store: Store::Ini,
            key,
            value,
        })
    });
    let mut entries: Vec<Entry> = launch.chain(ini).collect();
    entries.sort_by_key(|entry| {
        (
            Category::ALL.iter().position(|category| *category == entry.category),
            entry.store == Store::Ini,
            entry.key.clone(),
        )
    });
    Ok(entries)
}

/// `key`'s value for `target`, `None` where it is unset: a launch key from its settings, any other
/// from its port ini. File I/O: off the UI thread.
///
/// # Errors
/// A key every title shares or one no game has, a title with no settings yet, or a file that can't
/// be read.
pub fn get(root: &Root, target: &Target, key: &str) -> Result<Option<String>, PerGameError> {
    if LauncherSettings::is_shared(key) {
        return Err(Refusal::SharedKey(key.to_owned()).into());
    }
    if !keys::is_launch_key(key) {
        keys::check_ini_key(key)?;
    }
    let files = files(root, target, false)?;
    if keys::is_launch_key(key) {
        let launch = LauncherSettings::read_at(&files.settings, true)?;
        return Ok(launch.values.get(key).map(|value| shown(key, value)));
    }
    Ok(Ini::load(&files.ini).get(key).map(str::to_owned))
}

/// Sets `key` of `target` to `text`, checked as an import checks it; empty removes it, so the
/// default applies again. File I/O: off the UI thread.
///
/// # Errors
/// A key every title shares or one no game has, a value it can't take, or a file that can't be read
/// or written.
pub fn set(root: &Root, target: &Target, key: &str, text: &str) -> Result<(), PerGameError> {
    if LauncherSettings::is_shared(key) {
        return Err(Refusal::SharedKey(key.to_owned()).into());
    }
    let value = if keys::is_launch_key(key) {
        if text.is_empty() {
            None
        } else {
            Some(keys::launch_value(key, text, root.library())?)
        }
    } else {
        keys::check_ini_key(key)?;
        if !text.is_empty() {
            keys::check_ini(key, text)?;
        }
        None
    };
    let files = files(root, target, true)?;
    if keys::is_launch_key(key) {
        let mut launch = LauncherSettings::read_at(&files.settings, true)?;
        match value {
            Some(value) => launch.values.insert(key.to_owned(), value),
            None => launch.values.remove(key),
        };
        launch.write_at(root, &files.settings)?;
    } else {
        let mut ini = Ini::load(&files.ini);
        if text.is_empty() {
            ini.remove(key);
        } else {
            ini.set(key, text);
        }
        ini.save(&files.ini)?;
    }
    tracing::info!(
        target = target.name(),
        key,
        "game setting changed from the command line"
    );
    Ok(())
}

/// A launch value as text: a string as it is, anything else as JSON; `extra_env` without the
/// shadNet secrets a user may have typed in it.
fn shown(key: &str, value: &Value) -> String {
    match value {
        Value::String(text) if key == "extra_env" => keys::without_secrets(text),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
