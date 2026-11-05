//! Games Steam already has on this machine — installed ones from each
//! library's `appmanifest_*.acf` and the user's non-Steam shortcuts — found
//! so each can be added as an external game, or an installed one as a Steam
//! entry that Steam itself launches ([`entries`]), with the art Steam keeps
//! locally. Read-only: nothing is written into Steam's directories and no
//! image is downloaded.

pub mod entries;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use decanter_core::steam::{self, InstalledApp, SteamArt};
use decanter_core::{GameArt, platform};

use crate::art::file_url;

/// A Steam app id as Steam's files write it, checked once where it is read
/// so that it can go into a `steam://` URL and a manifest name as it is:
/// decimal digits only (no sign, no spaces) for a non-zero 32-bit id.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppId(u32);

#[derive(Debug, thiserror::Error)]
#[error("not a steam app id")]
pub struct InvalidAppId;

impl FromStr for AppId {
    type Err = InvalidAppId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        // `u32::from_str` takes a leading `+`, which no file of Steam's writes.
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(InvalidAppId);
        }
        text.parse().ok().filter(|id| *id != 0).map(Self).ok_or(InvalidAppId)
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone)]
pub struct SteamGame {
    /// `None` for a non-Steam shortcut, which only runs by its executable.
    pub app_id: Option<AppId>,
    pub name: String,
    /// The folder the game lives in; for a shortcut, its start directory.
    pub folder: PathBuf,
    /// The one program that is plainly the game, when there is one.
    pub executable: Option<PathBuf>,
    pub art: GameArt,
}

/// Every Steam game not yet covered by `added` (the executables of the
/// external games already in the library) nor by `entries` (the Steam
/// entries already there), by name. Blocking: reads manifests, directories
/// and `shortcuts.vdf`.
pub fn detect(added: &HashSet<PathBuf>, entries: &HashSet<AppId>) -> Vec<SteamGame> {
    let Some(root) = root() else {
        return Vec::new();
    };
    let mut found: Vec<SteamGame> = installed(&root)
        .filter(|(app_id, _)| !entries.contains(app_id))
        .map(|(app_id, app)| SteamGame {
            app_id: Some(app_id),
            name: app.name,
            executable: super::launchers::main_executable(&app.folder),
            art: game_art(app.art),
            folder: app.folder,
        })
        .collect();
    found.extend(shortcuts(&root));
    found.retain(|game| match &game.executable {
        Some(executable) => !added.contains(executable),
        None => !added.iter().any(|path| path.starts_with(&game.folder)),
    });
    found.sort_by_cached_key(|game| game.name.to_lowercase());
    tracing::debug!(root = %root.display(), count = found.len(), "detected steam games");
    found
}

/// The folder of every app Steam has fully installed right now, by id:
/// what tells whether a Steam entry is installed, read again each time
/// since Steam installs and removes them without decanter.
pub fn installed_apps() -> HashMap<AppId, PathBuf> {
    root()
        .map(|root| installed(&root).map(|(app_id, app)| (app_id, app.folder)).collect())
        .unwrap_or_default()
}

fn root() -> Option<PathBuf> {
    platform::steam_root_candidates().find(|root| root.join("steamapps").is_dir())
}

fn installed(root: &Path) -> impl Iterator<Item = (AppId, InstalledApp)> {
    steam::installed_apps(root)
        .into_iter()
        .filter_map(|app| Some((app.app_id.parse().ok()?, app)))
}

fn shortcuts(root: &Path) -> Vec<SteamGame> {
    steam::non_steam_shortcuts(root)
        .into_iter()
        .map(|shortcut| SteamGame {
            app_id: None,
            name: shortcut.name,
            folder: shortcut.start_dir,
            art: game_art(shortcut.art),
            executable: Some(shortcut.executable),
        })
        .collect()
}

/// The icon falls back to the cover when Steam keeps none.
fn game_art(art: SteamArt) -> GameArt {
    let url = |path: PathBuf| file_url(&path);
    GameArt {
        icon: art.icon.or_else(|| art.cover.clone()).map(url),
        hero: art.hero.map(url),
        cover: art.cover.map(url),
    }
}
