//! A PS game as a Steam non-Steam shortcut that starts `decanter ps run` for its title, written
//! through `decanter_core::steam` as a GOG game's is. The entry is found again by its launch
//! options rather than a `steam_shortcuts` row, since a PS game has no account or storefront to key
//! one by; a shortcut the user deleted in Steam then reads as gone, as it is.
//!
//! Best-effort like the GOG export: a missing Steam, an unresolved Steam user or art the dump lacks
//! costs the shortcut or one image, logged, never the action that asked for it.

use std::path::{Path, PathBuf};

use decanter_core::steam;
use decanter_ps::library::Game;
use decanter_ps::title_id::TitleId;

const RUN: &str = "ps run --title";

/// `Some(exported)` when a menu can offer the shortcut: one is there already, or a Steam user to
/// add it for. Reads `shortcuts.vdf`, so it's for a menu being opened, not one being drawn.
pub fn exported(title_id: &str) -> Option<bool> {
    let steam_root = steam::steam_root()?;
    let user_id = steam::local_user_id(&steam_root)?;
    Some(own_appid(&steam_root, &user_id, title_id).is_some())
}

/// Adds the shortcut once a dump lands in the library, unless the Steam export is off in Ajustes,
/// as a GOG game's install does.
pub async fn export_if_enabled(game: Game) {
    let Ok(db) = crate::core::db() else {
        return;
    };
    // Writes into the real Steam user's own config files, so the Ajustes toggle that turns the GOG
    // export off turns this one off too.
    if !db.steam_export_enabled().unwrap_or(true) {
        tracing::debug!(title_id = game.title_id, "steam export disabled in settings, skipping");
        return;
    }
    export(game).await;
}

/// Writes or updates the game's shortcut; the menu's "Add to Steam" comes here whatever the Ajustes
/// toggle says, since it is an explicit request.
pub async fn export(game: Game) {
    let title_id = match game.title_id.parse::<TitleId>() {
        Ok(title_id) => title_id,
        Err(err) => {
            tracing::warn!(%err, "PS game not exported to steam, its title id can't name a command");
            return;
        }
    };
    let Some(steam_root) = steam::steam_root() else {
        tracing::debug!(
            title_id = title_id.as_str(),
            "no Steam installation found, skipping Steam export"
        );
        return;
    };
    let Some(user_id) = steam::local_user_id(&steam_root) else {
        tracing::warn!(
            title_id = title_id.as_str(),
            "no local Steam user resolved, PS game not exported to steam"
        );
        return;
    };
    let Some(command) = crate::launcher::Launcher::ps(title_id.as_str()).and_then(|launcher| launcher.steam()) else {
        return;
    };
    let name = crate::state::ps_display_title(&game);
    let art = |file: &str| regular_file(&game.dir.join("sce_sys").join(file));
    let artwork = steam::Artwork {
        icon: art("icon0.png"),
        capsule: None,
        portrait: None,
        hero: art("pic1.png"),
    };
    let title = title_id.as_str().to_owned();
    let written = tokio::task::spawn_blocking(move || {
        let earlier = own_appid(&steam_root, &user_id, &title);
        let spec = steam::ShortcutSpec {
            name: &name,
            command: &command,
        };
        let appid = steam::sync(&steam_root, &user_id, &spec, &artwork)?;
        // The appid hashes the program's path and the name, so a moved AppImage, an entry from
        // before shortcuts started the CLI through its loader, or a renamed dump would otherwise
        // leave the old entry beside the new one.
        if let Some(earlier) = earlier.filter(|earlier| *earlier != appid) {
            steam::remove(&steam_root, &user_id, earlier)?;
        }
        Ok::<_, std::io::Error>(appid)
    })
    .await;
    match written {
        Ok(Ok(appid)) => tracing::info!(title_id = title_id.as_str(), appid, "synced PS game steam shortcut"),
        Ok(Err(err)) => tracing::warn!(%err, "failed to export PS game steam shortcut"),
        Err(err) => tracing::warn!(%err, "PS game steam shortcut worker task panicked"),
    }
}

/// Removes the game's shortcut and its art; nothing to do when there is none.
pub async fn remove(title_id: String) {
    let Some(steam_root) = steam::steam_root() else {
        return;
    };
    let Some(user_id) = steam::local_user_id(&steam_root) else {
        return;
    };
    let removed = tokio::task::spawn_blocking(move || {
        let Some(appid) = own_appid(&steam_root, &user_id, &title_id) else {
            return Ok(None);
        };
        steam::remove(&steam_root, &user_id, appid).map(|()| Some(appid))
    })
    .await;
    match removed {
        Ok(Ok(Some(appid))) => tracing::info!(appid, "removed PS game steam shortcut"),
        Ok(Ok(None)) => tracing::debug!("no PS game steam shortcut to remove"),
        Ok(Err(err)) => tracing::warn!(%err, "failed to remove PS game steam shortcut"),
        Err(err) => tracing::warn!(%err, "PS game steam shortcut worker task panicked"),
    }
}

fn own_appid(steam_root: &Path, user_id: &str, title_id: &str) -> Option<i32> {
    steam::find(steam_root, user_id, |options| title_of(options) == Some(title_id))
        .inspect_err(|err| tracing::warn!(%err, "could not read steam shortcuts"))
        .ok()
        .flatten()
}

/// The title a shortcut's launch options run, `ps run --title <id>` at their end.
pub fn title_of(options: &str) -> Option<&str> {
    let (rest, title) = options.rsplit_once(' ')?;
    let before = rest.strip_suffix(RUN)?;
    (before.is_empty() || before.ends_with(' ')).then_some(title)
}

/// Art from the dump, which may hold anything: only a regular file there, never what a symlink
/// points to, is copied into Steam's grid folder.
fn regular_file(path: &Path) -> Option<PathBuf> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .map(|_| path.to_owned())
}
