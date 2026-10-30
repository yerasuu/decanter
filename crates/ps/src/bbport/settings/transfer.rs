//! One game's settings as a single JSON file, to keep or to take to another library or machine:
//! its own launch keys, port ini lines and mod and patch choices, never what every title shares
//! (the account, the dump folders, the port in use) nor a shadNet secret. An import is checked
//! whole before anything is written: its title id, every key against the allow-list and every
//! value against what the key takes, and no folder outside the library.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::keys::{self, ini_category, launch_category};
use super::per_game::{self, Content, DEFAULTS, PerGameError, Target};
use crate::bbport::port_ini;
use crate::bbport::tools::{ModProfile, PatchProfile};
use crate::paths::Root;
use crate::title_id::TitleId;

const KIND: &str = "decanter-ps-settings";
const VERSION: u64 = 1;
/// Far above any game's settings; a larger file is not one decanter wrote.
pub const MAX_BYTES: u64 = 256 * 1024;
const MAX_LAUNCH: usize = 256;
const MAX_INI: usize = 2048;
const MAX_CHOICES: usize = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    kind: String,
    version: u64,
    title_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    exported_on: String,
    #[serde(default)]
    settings: serde_json::Map<String, Value>,
    #[serde(default)]
    ini: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mods: Option<ModProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    patches: Option<PatchProfile>,
}

/// `target`'s settings as the file [`import`] reads. What it could not take back in is left out and
/// logged: a folder outside the library, a value edited by hand into one the key doesn't take.
/// File I/O: off the UI thread.
///
/// # Errors
/// A title with no settings yet, or a file that can't be read.
pub fn export(root: &Root, target: &Target) -> Result<String, PerGameError> {
    let content = Content::read(&per_game::files(root, target, false)?)?;
    let library = root.library();
    let mut skipped = Vec::new();
    let settings = content
        .launch
        .into_iter()
        .filter(|(key, _)| launch_category(key).is_some())
        .map(|(key, value)| {
            let value = match value.as_str() {
                Some(env) if key == "extra_env" => Value::String(keys::without_secrets(env)),
                Some(_) | None => value,
            };
            (key, value)
        })
        .filter(|(key, value)| {
            let is_kept = keys::check_launch(key, value, library).is_ok();
            if !is_kept {
                skipped.push(key.clone());
            }
            is_kept
        })
        .collect();
    let ini = content
        .ini
        .into_iter()
        .filter(|(key, _)| ini_category(key).is_some())
        .filter(|(key, value)| {
            let is_kept = keys::check_ini(key, value).is_ok();
            if !is_kept {
                skipped.push(key.clone());
            }
            is_kept
        })
        .collect();
    if !skipped.is_empty() {
        tracing::warn!(target = target.name(), keys = ?skipped, "settings left out of the export, since an import would refuse them");
    }
    let (mods, patches) = content.choices.unzip();
    let export = Export {
        kind: KIND.to_owned(),
        version: VERSION,
        title_id: target.name().to_owned(),
        exported_on: port_ini::today(),
        settings,
        ini,
        mods,
        patches,
    };
    let mut text = serde_json::to_string_pretty(&export).map_err(io::Error::other)?;
    text.push('\n');
    tracing::info!(target = target.name(), "game settings exported");
    Ok(text)
}

/// Reads `path`, at most [`MAX_BYTES`] of it, and imports it into `target` ([`import`]).
///
/// # Errors
/// As [`import`], or the file that can't be read.
pub fn import_file(root: &Root, target: &Target, path: &Path, is_any_title: bool) -> Result<PathBuf, PerGameError> {
    import(root, target, &read(path)?, is_any_title)
}

/// The checks of [`import_file`] without writing anything, for what asks before importing: another
/// title's file comes back as [`PerGameError::OtherTitle`] unless `is_any_title`.
///
/// # Errors
/// As [`import_file`], but never writing.
pub fn check_file(root: &Root, target: &Target, path: &Path, is_any_title: bool) -> Result<(), PerGameError> {
    parse(root, target, &read(path)?, is_any_title).map(drop)
}

/// The port ini keys of an exported file, for what checks them against the port's own list before
/// an import; the file's other checks are [`check_file`]'s.
///
/// # Errors
/// A file that can't be read or isn't an export.
pub fn ini_keys(path: &Path) -> Result<Vec<String>, PerGameError> {
    let export: Export = serde_json::from_slice(&read(path)?).map_err(|_| keys::Refusal::NotExport)?;
    Ok(export.ini.into_keys().collect())
}

fn read(path: &Path) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    fs::File::open(path)?
        .take(MAX_BYTES.saturating_add(1))
        .read_to_end(&mut data)?;
    Ok(data)
}

/// Replaces `target`'s settings with those of an exported file, after a backup; returns the
/// backup's folder. A file of another title is refused unless `is_any_title`, the user's explicit
/// "apply to this game". Mod and patch choices apply only to a title, since the defaults hold none.
/// File I/O: off the UI thread.
///
/// # Errors
/// A file that isn't an export or fails a check ([`super::keys::Refusal`]), another title's file,
/// or a file that can't be read or written.
pub fn import(root: &Root, target: &Target, data: &[u8], is_any_title: bool) -> Result<PathBuf, PerGameError> {
    let content = parse(root, target, data, is_any_title)?;
    let backup = per_game::replace(root, target, content, &keys::Category::ALL, Some("import".to_owned()))?;
    tracing::info!(target = target.name(), backup = %backup.display(), "game settings imported");
    Ok(backup)
}

fn parse(root: &Root, target: &Target, data: &[u8], is_any_title: bool) -> Result<Content, PerGameError> {
    if u64::try_from(data.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err(keys::Refusal::TooLarge(MAX_BYTES).into());
    }
    let export: Export = serde_json::from_slice(data).map_err(|_| keys::Refusal::NotExport)?;
    if export.kind != KIND {
        return Err(keys::Refusal::NotExport.into());
    }
    if export.version > VERSION {
        return Err(keys::Refusal::Version(export.version).into());
    }
    if export.title_id != DEFAULTS && export.title_id.parse::<TitleId>().is_err() {
        return Err(keys::Refusal::TitleId(export.title_id.escape_default().to_string()).into());
    }
    if export.title_id != target.name() && !is_any_title {
        return Err(PerGameError::OtherTitle {
            file: export.title_id,
            target: target.name().to_owned(),
        });
    }
    Ok(checked(root, export)?)
}

fn checked(root: &Root, export: Export) -> Result<Content, keys::Refusal> {
    let mod_names: Vec<&String> = export
        .mods
        .iter()
        .flat_map(|mods| mods.order.iter().chain(&mods.disabled))
        .collect();
    let patch_keys: Vec<&String> = export
        .patches
        .iter()
        .flat_map(|patches| patches.enabled.iter().chain(&patches.disabled))
        .collect();
    if export.settings.len() > MAX_LAUNCH
        || export.ini.len() > MAX_INI
        || mod_names.len() > MAX_CHOICES
        || patch_keys.len() > MAX_CHOICES
    {
        return Err(keys::Refusal::TooMany);
    }
    for (key, value) in &export.settings {
        keys::check_launch(key, value, root.library())?;
    }
    for (key, value) in &export.ini {
        keys::check_ini(key, value)?;
    }
    for name in mod_names {
        keys::check_name(name, false)?;
    }
    for key in patch_keys {
        keys::check_name(key, true)?;
    }
    let choices = (export.mods.is_some() || export.patches.is_some())
        .then(|| (export.mods.unwrap_or_default(), export.patches.unwrap_or_default()));
    Ok(Content {
        launch: export.settings,
        ini: export.ini.into_iter().collect(),
        choices,
    })
}
