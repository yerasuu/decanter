//! What a launch gets from the [`GameTool`]s its game or its
//! library turned on, the same for a Proton game, the CLI and a PS port.

use std::path::{Path, PathBuf};

use decanter_core::{Db, GameTool};

/// `Some(autostart_log)` when `game_key` starts with MangoHud. Turned on but
/// not installed starts the game without it, said in the log.
pub fn mangohud(db: &Db, game_key: &str) -> Option<bool> {
    match db.game_tool_enabled(game_key, GameTool::MangoHud) {
        Ok(true) if decanter_core::mangohud_installed() => Some(db.mangohud_autostart_log().unwrap_or_else(|err| {
            tracing::warn!(game_key, error = %err, "failed to read the MangoHud log setting, not logging from the start");
            false
        })),
        Ok(true) => {
            tracing::warn!(
                game_key,
                "MangoHud is on for the game but its Vulkan layer isn't installed, starting without it"
            );
            None
        }
        Ok(false) => None,
        Err(err) => {
            tracing::warn!(game_key, error = %err, "failed to read the MangoHud choice, starting without it");
            None
        }
    }
}

/// The `gamemoded` to start `game_key` through, when it starts under
/// GameMode. Turned on but not installed starts the game without it, said
/// in the log.
pub fn gamemoded(db: &Db, game_key: &str) -> Option<PathBuf> {
    match db.game_tool_enabled(game_key, GameTool::GameMode) {
        Ok(true) => {
            let gamemoded = decanter_core::gamemoded();
            match &gamemoded {
                Some(path) => tracing::info!(game_key, gamemoded = %path.display(), "starting under GameMode"),
                None => tracing::warn!(
                    game_key,
                    "GameMode is on for the game but gamemoded or its D-Bus service isn't installed, starting without it"
                ),
            }
            gamemoded
        }
        Ok(false) => None,
        Err(err) => {
            tracing::warn!(game_key, error = %err, "failed to read the GameMode choice, starting without it");
            None
        }
    }
}

/// [`decanter_core::mangohud_env`] with its log folder made in `library_root`.
pub async fn mangohud_env(game_key: &str, library_root: &Path, autostart_log: bool) -> [(&'static str, String); 2] {
    let log_dir = decanter_core::mangohud_log_dir(library_root);
    if let Err(err) = tokio::fs::create_dir_all(&log_dir).await {
        tracing::warn!(game_key, log_dir = %log_dir.display(), %err, "cannot create MangoHud's log folder");
    }
    tracing::info!(game_key, log_dir = %log_dir.display(), autostart_log, "starting with MangoHud");
    decanter_core::mangohud_env(&log_dir, autostart_log)
}
