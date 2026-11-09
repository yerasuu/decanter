//! decanter's side of third-party mods (`decanter_mods`): which folder a game's mods are in, and
//! the play tree a launch starts the game from.

use std::path::{Path, PathBuf};

use decanter_core::{InstalledGame, Storefront};
use decanter_mods::{GameMods, Prepared, Store};

/// Where pressure-vessel shares the host's files with the game by default, per Valve's
/// [Steam Linux Runtime known issues](https://github.com/ValveSoftware/steam-runtime/blob/master/doc/steamlinuxruntime-known-issues.md).
const SHARED_BY_DEFAULT: [&str; 6] = ["/home", "/media", "/mnt", "/run/media", "/opt", "/srv"];
const SHARE_VARIABLE: &str = "PRESSURE_VESSEL_FILESYSTEMS_RW";

/// The mods folder of an installed GOG, Amazon Games or Epic Games game, all of which install
/// under `common/<InstallDir>` with a manifest of their own. External games get mods later
/// (design, open question 6), so they have none.
pub fn for_game(game: &InstalledGame) -> Option<GameMods> {
    let store = match game.storefront {
        Storefront::Gog => Store::Gog,
        Storefront::AmazonGames => Store::Amazon,
        Storefront::EpicGames => Store::Epic,
        _ => return None,
    };
    let library = match crate::core::library_for_account(&game.account_id) {
        Ok(library) => library,
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "no library, no mods folder");
            return None;
        }
    };
    let install = library.install_dir(&game.install_dir)?;
    GameMods::new(&install, store, &game.game_id)
        .inspect_err(|err| tracing::warn!(game_id = %game.game_id, %err, "no mods folder for the game"))
        .ok()
}

/// The DLL overrides the account's enabled mods give Wine, as `dll=mode (mod)` for Compatibilidad,
/// which shows everything that changes how Wine loads the game; `None` when there are none.
pub async fn override_summary(account_id: &str, game: &InstalledGame) -> Option<String> {
    let mods = for_game(game)?;
    let account = account_id.to_owned();
    let read = tokio::task::spawn_blocking(move || {
        let names = mods.list()?;
        let overrides = mods.dll_overrides(&account)?;
        Ok::<_, decanter_mods::Error>(
            overrides
                .iter()
                .map(|wanted| {
                    let name = names
                        .iter()
                        .find(|manifest| manifest.id == wanted.from)
                        .map_or_else(|| wanted.from.to_string(), |manifest| manifest.name.clone());
                    format!("{}={} ({name})", wanted.dll, wanted.mode)
                })
                .collect::<Vec<_>>(),
        )
    })
    .await;
    match read {
        Ok(Ok(lines)) if lines.is_empty() => None,
        Ok(Ok(lines)) => Some(lines.join(", ")),
        Ok(Err(err)) => {
            tracing::warn!(game_id = %game.game_id, %err, "the mods' dll overrides could not be read");
            None
        }
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "reading the mods' dll overrides stopped");
            None
        }
    }
}

/// How many of the account's enabled mods an update of the game changed a file under, which are
/// left out of the play tree until the Mods tab keeps or disables them; checked right after the
/// update so the Updates tab can say so once.
pub async fn check_after_update(account_id: &str, game: &InstalledGame) -> usize {
    let Some(mods) = for_game(game) else {
        return 0;
    };
    let account = account_id.to_owned();
    let build_id = game.build_id.clone();
    match tokio::task::spawn_blocking(move || mods.needing_attention(&account, &build_id)).await {
        Ok(Ok(attention)) => {
            if !attention.is_empty() {
                tracing::warn!(game_id = %game.game_id, mods = attention.len(), "the update changed game files that mods replace");
            }
            attention.len()
        }
        Ok(Err(err)) => {
            tracing::warn!(game_id = %game.game_id, %err, "mods not checked after the update");
            0
        }
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "checking the mods after the update stopped");
            0
        }
    }
}

/// What uninstalling a game removes of its mods, for the confirmation (decision #1133: say what
/// goes and how big it is).
#[derive(Clone, Copy)]
pub struct UninstallMods {
    /// What the game wrote into this account's play tree, which goes with the prefix.
    pub play_bytes: u64,
    pub mods: usize,
    pub mods_bytes: u64,
    /// Only when no other account still has the game installed, since its set may use them.
    pub can_remove_mods: bool,
}

/// Whether an uninstall also deletes the game's mods: off unless ticked (design, open question 9).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OnUninstall {
    KeepMods,
    RemoveMods,
}

/// `None` when the game has no mods folder.
pub async fn uninstall_summary(account_id: &str, game: &InstalledGame) -> Option<UninstallMods> {
    let mods = for_game(game)?;
    if !mods.dir().exists() {
        return None;
    }
    // Without the library, taken as shared so that nothing another account may use is offered.
    let shared = crate::core::library_for_account(account_id).map_or(true, |library| {
        library.install_dir_is_shared(&game.install_dir, account_id, &game.game_id)
    });
    let account = account_id.to_owned();
    let read = tokio::task::spawn_blocking(move || {
        let all = mods.list()?;
        let play_bytes = mods.play_size(&account)?;
        Ok::<_, decanter_mods::Error>(UninstallMods {
            play_bytes,
            mods: all.len(),
            mods_bytes: all.iter().map(decanter_mods::ModManifest::size).sum(),
            can_remove_mods: !shared,
        })
    })
    .await;
    match read {
        Ok(Ok(summary)) => Some(summary),
        Ok(Err(err)) => {
            tracing::warn!(game_id = %game.game_id, %err, "the game's mods could not be measured for the uninstall");
            None
        }
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "measuring the game's mods stopped");
            None
        }
    }
}

/// After the game's own uninstall: its play tree for the account goes, and its mods when asked.
pub async fn after_uninstall(account_id: &str, mods: Option<GameMods>, choice: OnUninstall) -> Result<(), String> {
    let Some(mods) = mods else {
        return Ok(());
    };
    let account = account_id.to_owned();
    tokio::task::spawn_blocking(move || {
        mods.remove_play(&account)?;
        if choice == OnUninstall::RemoveMods {
            mods.remove_all()?;
        }
        Ok::<_, decanter_mods::Error>(())
    })
    .await
    .map_err(|err| format!("removing the game's mods stopped: {err}"))?
    .map_err(|err| format!("the game is uninstalled, but its mods could not be removed: {err}"))
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
    let build_id = game.build_id.clone();
    let install = mods.install().to_path_buf();
    let prepared = tokio::task::spawn_blocking(move || mods.prepare(&account, &build_id))
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

    pub fn facts(&self) -> [(&'static str, String); 4] {
        let names = self
            .prepared
            .mods
            .iter()
            .map(|(id, name)| format!("{name} ({id})"))
            .collect::<Vec<_>>()
            .join(", ");
        let left_out = self
            .prepared
            .left_out
            .iter()
            .map(|(id, name)| format!("{name} ({id}), an update changed a game file it replaces"))
            .collect::<Vec<_>>()
            .join(", ");
        let left_out = if left_out.is_empty() {
            "none".to_owned()
        } else {
            left_out
        };
        let overrides = self
            .prepared
            .dll_overrides
            .iter()
            .map(|wanted| format!("{}={}", wanted.dll, wanted.mode))
            .collect::<Vec<_>>()
            .join(";");
        [
            ("mods", if names.is_empty() { "none".to_owned() } else { names }),
            ("play_root", self.prepared.root.display().to_string()),
            (
                "mod_dll_overrides",
                if overrides.is_empty() {
                    "none".to_owned()
                } else {
                    overrides
                },
            ),
            ("mods_left_out", left_out),
        ]
    }

    /// The enabled mods' DLL overrides as `Launch::dll_overrides` takes them, after the launch's own
    /// so that a mod's proxy wins.
    pub fn dll_overrides(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.prepared
            .dll_overrides
            .iter()
            .map(|wanted| (wanted.dll.clone(), wanted.mode.as_str().to_owned()))
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
