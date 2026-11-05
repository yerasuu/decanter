//! Games Steam already has on this machine — installed ones from each
//! library's `appmanifest_*.acf` and the user's non-Steam shortcuts — found
//! so each can be added as an external game, with the art Steam keeps
//! locally. Read-only: nothing is written into Steam's directories and no
//! image is downloaded.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use decanter_core::steam::{self, SteamArt};
use decanter_core::{GameArt, platform};

use crate::art::file_url;

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

fn installed(root: &Path) -> Vec<SteamGame> {
    steam::installed_apps(root)
        .into_iter()
        .map(|app| SteamGame {
            name: app.name,
            executable: super::launchers::main_executable(&app.folder),
            art: game_art(app.art),
            folder: app.folder,
        })
        .collect()
}

fn shortcuts(root: &Path) -> Vec<SteamGame> {
    steam::non_steam_shortcuts(root)
        .into_iter()
        .map(|shortcut| SteamGame {
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
