//! Games Steam already has on this machine — installed ones from each
//! library's `appmanifest_*.acf` and the user's non-Steam shortcuts — found
//! so each can be added as an external game, with the art Steam keeps
//! locally. Read-only: nothing is written into Steam's directories and no
//! image is downloaded.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use decanter_core::{GameArt, platform, steam, vdf};

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

#[derive(Clone)]
pub struct SteamGame {
    pub name: String,
    /// The folder the game lives in; for a shortcut, its start directory.
    pub folder: PathBuf,
    /// The one program that is plainly the game, when there is one.
    pub executable: Option<PathBuf>,
    pub art: GameArt,
}

/// Every Steam game not yet covered by `added` (the executables of the
/// external games already in the library), by name. Blocking: reads
/// manifests, directories and `shortcuts.vdf`.
pub fn detect(added: &HashSet<PathBuf>) -> Vec<SteamGame> {
    let Some(root) = platform::steam_root_candidates().find(|root| root.join("steamapps").is_dir()) else {
        return Vec::new();
    };
    let mut found = installed(&root);
    found.extend(shortcuts(&root));
    found.retain(|game| match &game.executable {
        Some(executable) => !added.contains(executable),
        None => !added.iter().any(|path| path.starts_with(&game.folder)),
    });
    found.sort_by_cached_key(|game| game.name.to_lowercase());
    tracing::debug!(root = %root.display(), count = found.len(), "detected steam games");
    found
}

/// Steam's library folders: its own, plus every one in `libraryfolders.vdf`
/// (the SD card's among them). Each entry is checked before it is used as a
/// path: absolute, and a directory with a `steamapps` in it.
fn libraries(root: &Path) -> Vec<PathBuf> {
    let listed = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf"))
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
        .filter(|path| seen.insert(std::fs::canonicalize(path).unwrap_or_else(|_| path.clone())))
        .collect()
}

fn installed(root: &Path) -> Vec<SteamGame> {
    let cache = root.join("appcache/librarycache");
    let mut games = Vec::new();
    for library in libraries(root) {
        let steamapps = library.join("steamapps");
        let Ok(entries) = std::fs::read_dir(&steamapps) else {
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
            if let Some(game) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| manifest(&text, app_id, &steamapps, &cache))
            {
                games.push(game);
            }
        }
    }
    games
}

/// One `appmanifest`, if it is a fully installed game whose folder is where
/// it says. `installdir` is a name Steam writes, but the file sits where
/// users edit it: it has to be a single plain component to be joined onto
/// `steamapps/common`.
fn manifest(text: &str, file_app_id: &str, steamapps: &Path, cache: &Path) -> Option<SteamGame> {
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
    Some(SteamGame {
        name: name.to_owned(),
        executable: super::launchers::main_executable(&folder),
        art: library_art(cache, file_app_id),
        folder,
    })
}

/// Steam's `librarycache` has held an app's images two ways: flat as
/// `<appid>_library_600x900.jpg`, and in `<appid>/`, directly or one hash
/// folder down.
fn library_art(cache: &Path, app_id: &str) -> GameArt {
    let dir = cache.join(app_id);
    let subfolders: Vec<PathBuf> = std::fs::read_dir(&dir)
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
    let cover = find("library_600x900");
    GameArt {
        icon: find("icon").or_else(|| cover.clone()).map(url),
        hero: find("library_hero").map(url),
        cover: cover.map(url),
    }
}

/// The user's non-Steam shortcuts that decanter didn't export itself: an
/// absolute `Exe` that exists, with the grid art Steam keeps for it.
fn shortcuts(root: &Path) -> Vec<SteamGame> {
    let Some(user) = steam::local_user_id(root) else {
        return Vec::new();
    };
    let path = steam::shortcuts_vdf_path(root, &user);
    let Some((_, entries)) = std::fs::read(&path)
        .ok()
        .and_then(|bytes| steam::binary_vdf::decode(&bytes).ok())
    else {
        return Vec::new();
    };
    let grid = steam::grid_dir(root, &user);
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
                .is_some_and(|tags| tags.iter().any(|(_, tag)| tag.as_str() == Some(steam::export::TAG)));
            if name.is_empty() || exported || !executable.is_absolute() || !executable.is_file() {
                return None;
            }
            let id = steam::grid_appid(entry.get("appid")?.as_int()?);
            let find = |suffix: &str| {
                IMAGE_EXTENSIONS
                    .iter()
                    .map(|extension| grid.join(format!("{id}{suffix}.{extension}")))
                    .find(|path| path.is_file())
            };
            let cover = find("p");
            Some(SteamGame {
                name: name.to_owned(),
                folder: executable.parent()?.to_path_buf(),
                art: GameArt {
                    icon: find("_icon").or_else(|| cover.clone()).map(url),
                    hero: find("_hero").map(url),
                    cover: cover.map(url),
                },
                executable: Some(executable),
            })
        })
        .collect()
}

fn url(path: PathBuf) -> String {
    format!("file://{}", path.display())
}
