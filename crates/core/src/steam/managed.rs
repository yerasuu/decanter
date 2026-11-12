//! What Steam keeps for one of its own apps — the compatibility tool picked for it, its launch
//! options, and what its `appmanifest` says — read to be shown, never changed: Steam owns all of
//! it. Every file here is one users edit by hand, so each value is taken only when it reads as
//! what Steam writes, and a missing or unreadable one is left out.

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

use std::fs;
use std::path::{Path, PathBuf};

use super::library::{install_folder, libraries, library_art, library_cache};
use crate::vdf;

/// `CompatToolMapping`'s key for the tool Steam Play uses for every app without its own.
const DEFAULT_TOOL_KEY: &str = "0";

/// The branch Steam installs when the user picked none.
const DEFAULT_BETA: &str = "public";

/// Which compatibility tool Steam runs an app with.
#[derive(Clone)]
pub enum CompatTool {
    /// Picked for this app.
    Own(String),
    /// Steam Play's default for every app without its own.
    Default(String),
    None,
}

/// Steam's settings and install facts for one app, each `None` when Steam has no readable value.
#[derive(Clone, Default)]
pub struct Managed {
    pub compat_tool: Option<CompatTool>,
    pub launch_options: Option<String>,
    pub language: Option<String>,
    pub beta: Option<String>,
    pub folder: Option<PathBuf>,
    pub size_on_disk: Option<u64>,
    /// Unix seconds.
    pub last_updated: Option<i64>,
    pub icon: Option<PathBuf>,
}

/// Everything Steam under `root` keeps for `app_id`. `app_id` must be decimal digits only, since it
/// goes into a file name; any other is read as an app Steam has nothing for. Blocking: reads
/// `config.vdf`, `localconfig.vdf` and the app's manifest.
#[must_use]
pub fn managed(root: &Path, app_id: &str) -> Managed {
    if app_id.is_empty() || !app_id.bytes().all(|byte| byte.is_ascii_digit()) {
        tracing::debug!(app_id, "not a steam app id, nothing managed to read");
        return Managed::default();
    }
    let mut managed = Managed {
        compat_tool: compat_tool(root, app_id),
        launch_options: launch_options(root, app_id),
        icon: library_art(&library_cache(root), app_id).icon,
        ..Managed::default()
    };
    if let Some((state, steamapps)) = app_manifest(root, app_id) {
        let user_config = state.get_block("UserConfig");
        managed.language = user_config
            .and_then(|config| config.get("language"))
            .filter(|language| !language.is_empty())
            .map(str::to_owned);
        managed.beta = Some(
            user_config
                .and_then(|config| config.get("BetaKey"))
                .filter(|beta| !beta.is_empty())
                .unwrap_or(DEFAULT_BETA)
                .to_owned(),
        );
        managed.folder = install_folder(&state, &steamapps);
        managed.size_on_disk = state.get_u64("SizeOnDisk");
        managed.last_updated = state
            .get_u64("LastUpdated")
            .filter(|seconds| *seconds > 0)
            .and_then(|seconds| i64::try_from(seconds).ok());
    }
    managed
}

/// Parses the VDF file at `path`, logging at debug why one can't be read.
fn read_vdf(path: &Path) -> Option<vdf::Value> {
    let text = fs::read_to_string(path)
        .inspect_err(|err| tracing::debug!(path = %path.display(), %err, "cannot read steam file"))
        .ok()?;
    vdf::from_str(&text)
        .inspect_err(|err| tracing::debug!(path = %path.display(), %err, "cannot parse steam file"))
        .ok()
        .map(|(_, value)| value)
}

/// Follows `keys` down from `value`, ignoring case, since Steam's `KeyValues` does and its own
/// files have spelled `Valve` both ways.
fn descend<'value>(value: &'value vdf::Value, keys: &[&str]) -> Option<&'value vdf::Value> {
    keys.iter().try_fold(value, |node, key| {
        node.as_block()?
            .iter()
            .find(|(name, child)| name.eq_ignore_ascii_case(key) && child.as_block().is_some())
            .map(|(_, child)| child)
    })
}

/// `config.vdf`'s `CompatToolMapping`: the app's own tool, else Steam Play's default, else none.
fn compat_tool(root: &Path, app_id: &str) -> Option<CompatTool> {
    let config = read_vdf(&root.join("config").join("config.vdf"))?;
    let Some(mapping) = descend(&config, &["Software", "Valve", "Steam", "CompatToolMapping"]) else {
        return Some(CompatTool::None);
    };
    let tool = |key: &str| {
        mapping
            .get_block(key)?
            .get("name")
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    };
    Some(
        tool(app_id)
            .map(CompatTool::Own)
            .or_else(|| tool(DEFAULT_TOOL_KEY).map(CompatTool::Default))
            .unwrap_or(CompatTool::None),
    )
}

/// The launch options in the most recently changed `userdata/<id>/config/localconfig.vdf`: with
/// several Steam users on the machine, the last one Steam wrote for is the one playing.
fn launch_options(root: &Path, app_id: &str) -> Option<String> {
    let path = fs::read_dir(root.join("userdata"))
        .inspect_err(|err| tracing::debug!(root = %root.display(), %err, "cannot list steam userdata"))
        .ok()?
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit()))
        })
        .map(|entry| entry.path().join("config").join("localconfig.vdf"))
        .filter_map(|path| {
            let modified = fs::metadata(&path).and_then(|metadata| metadata.modified()).ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)?;
    let config = read_vdf(&path)?;
    descend(&config, &["Software", "Valve", "Steam", "apps", app_id])?
        .get("LaunchOptions")
        .map(str::trim)
        .filter(|options| !options.is_empty())
        .map(str::to_owned)
}

/// `app_id`'s `appmanifest` in whichever of Steam's libraries has it, with that library's
/// `steamapps`, once the id written inside matches the file's name.
fn app_manifest(root: &Path, app_id: &str) -> Option<(vdf::Value, PathBuf)> {
    let found = libraries(root).into_iter().find_map(|library| {
        let steamapps = library.join("steamapps");
        let path = steamapps.join(format!("appmanifest_{app_id}.acf"));
        path.is_file().then(|| (path, steamapps))
    });
    let Some((path, steamapps)) = found else {
        tracing::debug!(root = %root.display(), app_id, "no steam appmanifest for app");
        return None;
    };
    let state = read_vdf(&path)?;
    if state.get("appid") != Some(app_id) {
        tracing::debug!(path = %path.display(), app_id, "steam appmanifest names another app");
        return None;
    }
    Some((state, steamapps))
}
