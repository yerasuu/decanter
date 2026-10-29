//! decanter's side of third-party mods (`decanter_mods`): which folder a game's mods are in, and
//! the play tree a launch starts the game from.

use std::path::{Path, PathBuf};

use decanter_core::{InstalledGame, Storefront};
use decanter_mods::{GameMods, Prepared, Store};

/// Where pressure-vessel shares the host's files with the game by default, per Valve's
/// [Steam Linux Runtime known issues](https://github.com/ValveSoftware/steam-runtime/blob/master/doc/steamlinuxruntime-known-issues.md).
const SHARED_BY_DEFAULT: [&str; 6] = ["/home", "/media", "/mnt", "/run/media", "/opt", "/srv"];
const SHARE_VARIABLE: &str = "PRESSURE_VESSEL_FILESYSTEMS_RW";

/// The mods folder of an installed GOG game. External games get mods later (design, open
/// question 6), so they have none.
pub fn for_game(game: &InstalledGame) -> Option<GameMods> {
    if game.storefront != Storefront::Gog {
        return None;
    }
    let library = match crate::core::library_for_account(&game.account_id) {
        Ok(library) => library,
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "no library, no mods folder");
            return None;
        }
    };
    let install = library.install_dir(&game.install_dir)?;
    GameMods::new(&install, Store::Gog, &game.game_id)
        .inspect_err(|err| tracing::warn!(game_id = %game.game_id, %err, "no mods folder for the game"))
        .ok()
}

/// A launch from a play tree instead of the install.
pub struct Play {
    install: PathBuf,
    prepared: Prepared,
}

/// Brings the account's play tree up to date; `None` when the game starts from its install as it
/// always did. Blocking work, run off the UI.
pub async fn prepare(account_id: &str, game: &InstalledGame) -> Result<Option<Play>, String> {
    let Some(mods) = for_game(game) else {
        return Ok(None);
    };
    let account = account_id.to_owned();
    let install = mods.install().to_path_buf();
    let prepared = tokio::task::spawn_blocking(move || mods.prepare(&account))
        .await
        .map_err(|err| format!("the play tree's preparation stopped: {err}"))?
        .map_err(|err| format!("the game's mods could not be applied: {err}"))?;
    Ok(prepared.map(|prepared| Play { install, prepared }))
}

impl Play {
    /// `path` inside the install, at the same place in the play tree.
    pub fn rebase(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.install)
            .ok()
            .map(|relative| self.prepared.root.join(relative))
    }

    pub fn facts(&self) -> [(&'static str, String); 2] {
        let names = self
            .prepared
            .mods
            .iter()
            .map(|(id, name)| format!("{name} ({id})"))
            .collect::<Vec<_>>()
            .join(", ");
        [
            ("mods", if names.is_empty() { "none".to_owned() } else { names }),
            ("play_root", self.prepared.root.display().to_string()),
        ]
    }

    /// `game` with its cloud save folder moved into the play tree when it was inside the install,
    /// since the game now writes its saves there; `None` when nothing moves.
    pub fn saves_view(&self, game: &InstalledGame) -> Option<InstalledGame> {
        let folder = game.cloud_save_path.as_deref()?;
        let moved = self.rebase(Path::new(folder))?;
        let mut view = game.clone();
        view.cloud_save_path = Some(moved.display().to_string());
        Some(view)
    }

    /// Shares the library with pressure-vessel's container when it is outside the folders shared
    /// by default, since a link whose target the container can't see is a missing file to the game.
    pub fn share_env(&self) -> Option<(String, String)> {
        let root = self.install.parent().and_then(Path::parent)?;
        let home = dirs::home_dir();
        let shared = SHARED_BY_DEFAULT
            .iter()
            .map(Path::new)
            .chain(home.as_deref())
            .any(|base| root.starts_with(base));
        if shared {
            return None;
        }
        let root = root.display().to_string();
        let value = match std::env::var(SHARE_VARIABLE) {
            Ok(existing) if !existing.is_empty() => format!("{existing}:{root}"),
            Ok(_) | Err(_) => root,
        };
        Some((SHARE_VARIABLE.to_owned(), value))
    }
}
