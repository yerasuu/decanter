//! Reading what Steam itself keeps on this machine — the apps installed in
//! its libraries (`libraryfolders.vdf`, `appmanifest_*.acf`, the
//! `librarycache` art) and the user's non-Steam shortcuts with their grid
//! art. Read-only: nothing is written into Steam's directories.

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

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::{binary_vdf, export, grid_appid, grid_dir, local_user_id, shortcuts_vdf_path};
use crate::vdf;

/// `StateFlags` bit Steam sets once an app is fully installed.
const FULLY_INSTALLED: u64 = 4;

/// Steam apps that are tooling, not games.
const TOOL_PREFIXES: [&str; 4] = [
    "Proton",
    "Steam Linux Runtime",
    "Steamworks Common",
    "Steam Controller Configs",
];

const IMAGE_EXTENSIONS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// Image files Steam keeps locally for an app, each only when present.
#[derive(Clone, Default)]
pub struct SteamArt {
    pub icon: Option<PathBuf>,
    pub hero: Option<PathBuf>,
    pub cover: Option<PathBuf>,
}

/// A fully installed Steam app whose folder exists.
#[derive(Clone)]
pub struct InstalledApp {
    pub name: String,
    pub folder: PathBuf,
    pub art: SteamArt,
}

/// A non-Steam shortcut whose `Exe` is an existing absolute file.
#[derive(Clone)]
pub struct Shortcut {
    pub name: String,
    pub executable: PathBuf,
    pub start_dir: PathBuf,
    pub art: SteamArt,
}

/// Steam's library folders: its own, plus every one in `libraryfolders.vdf`
/// (the SD card's among them). Each entry is checked before it is used as a
/// path: absolute, and a directory with a `steamapps` in it.
fn libraries(root: &Path) -> Vec<PathBuf> {
    let listed = fs::read_to_string(root.join("steamapps/libraryfolders.vdf"))
        .inspect_err(|err| tracing::debug!(%err, "no libraryfolders.vdf to read"))
        .ok()
        .and_then(|text| {
            vdf::from_str(&text)
                .inspect_err(|err| tracing::warn!(%err, "failed to parse libraryfolders.vdf"))
                .ok()
        })
        .and_then(|(_, folders)| {
            folders.as_block().map(|block| {
                block
                    .values()
                    .filter_map(|entry| entry.get("path"))
                    .map(PathBuf::from)
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    let mut seen = HashSet::new();
    std::iter::once(root.to_path_buf())
        .chain(listed)
        .filter(|path| path.is_absolute() && path.join("steamapps").is_dir())
        .filter(|path| seen.insert(fs::canonicalize(path).unwrap_or_else(|_| path.clone())))
        .collect()
}

/// Every fully installed game across Steam's libraries, tooling excluded.
/// Blocking: reads manifests and directories.
#[must_use]
pub fn installed_apps(root: &Path) -> Vec<InstalledApp> {
    let cache = root.join("appcache/librarycache");
    let mut apps = Vec::new();
    for library in libraries(root) {
        let steamapps = library.join("steamapps");
        let Ok(entries) = fs::read_dir(&steamapps) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            let Some(app_id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("appmanifest_")?.strip_suffix(".acf"))
                .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
            else {
                continue;
            };
            if let Some(app) = fs::read_to_string(&path)
                .ok()
                .and_then(|text| manifest(&text, app_id, &steamapps, &cache))
            {
                apps.push(app);
            }
        }
    }
    apps
}

/// One `appmanifest`, if it is a fully installed game whose folder is where
/// it says. `installdir` is a name Steam writes, but the file sits where
/// users edit it: it has to be a single plain component to be joined onto
/// `steamapps/common`.
fn manifest(text: &str, file_app_id: &str, steamapps: &Path, cache: &Path) -> Option<InstalledApp> {
    let (_, state) = vdf::from_str(text).ok()?;
    if state.get("appid")? != file_app_id || state.get_u64("StateFlags")? & FULLY_INSTALLED == 0 {
        return None;
    }
    let name = state.get("name")?.trim();
    if name.is_empty() || TOOL_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
        return None;
    }
    let mut parts = Path::new(state.get("installdir")?).components();
    let (Some(Component::Normal(dir)), None) = (parts.next(), parts.next()) else {
        return None;
    };
    let folder = steamapps.join("common").join(dir);
    if !folder.is_dir() {
        return None;
    }
    Some(InstalledApp {
        name: name.to_owned(),
        art: library_art(cache, file_app_id),
        folder,
    })
}

/// Steam's `librarycache` has held an app's images two ways: flat as
/// `<appid>_library_600x900.jpg`, and in `<appid>/`, directly or one hash
/// folder down.
fn library_art(cache: &Path, app_id: &str) -> SteamArt {
    let dir = cache.join(app_id);
    let subfolders: Vec<PathBuf> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    let find = |name: &str| {
        IMAGE_EXTENSIONS.iter().find_map(|extension| {
            std::iter::once(cache.join(format!("{app_id}_{name}.{extension}")))
                .chain(std::iter::once(dir.join(format!("{name}.{extension}"))))
                .chain(
                    subfolders
                        .iter()
                        .map(|folder| folder.join(format!("{name}.{extension}"))),
                )
                .find(|path| path.is_file())
        })
    };
    SteamArt {
        icon: find("icon"),
        hero: find("library_hero"),
        cover: find("library_600x900"),
    }
}

/// The user's non-Steam shortcuts that decanter didn't export itself: an
/// absolute `Exe` that exists, with the grid art Steam keeps for it.
#[must_use]
pub fn non_steam_shortcuts(root: &Path) -> Vec<Shortcut> {
    let Some(user) = local_user_id(root) else {
        return Vec::new();
    };
    let path = shortcuts_vdf_path(root, &user);
    let Some((_, entries)) = fs::read(&path).ok().and_then(|bytes| binary_vdf::decode(&bytes).ok()) else {
        return Vec::new();
    };
    let grid = grid_dir(root, &user);
    entries
        .as_map()
        .into_iter()
        .flatten()
        .filter_map(|(_, entry)| {
            let name = entry.get("AppName")?.as_str()?.trim();
            let executable = PathBuf::from(entry.get("Exe")?.as_str()?.trim_matches('"'));
            let exported = entry
                .get("tags")?
                .as_map()
                .is_some_and(|tags| tags.iter().any(|(_, tag)| tag.as_str() == Some(export::TAG)));
            if name.is_empty() || exported || !executable.is_absolute() || !executable.is_file() {
                return None;
            }
            let id = grid_appid(entry.get("appid")?.as_int()?);
            let find = |suffix: &str| {
                IMAGE_EXTENSIONS
                    .iter()
                    .map(|extension| grid.join(format!("{id}{suffix}.{extension}")))
                    .find(|path| path.is_file())
            };
            Some(Shortcut {
                name: name.to_owned(),
                start_dir: executable.parent()?.to_path_buf(),
                art: SteamArt {
                    icon: find("_icon"),
                    hero: find("_hero"),
                    cover: find("p"),
                },
                executable,
            })
        })
        .collect()
}
