//! Steam entries: games of the user's Steam library shown in decanter and
//! started by Steam (`steam://rungameid/<appid>`), never by decanter, so
//! they get no prefix, Proton or umu settings of decanter's.
//!
//! Filed like an external game, under the profile, but as a
//! `Storefront::Steam` manifest whose id is the app id. The manifest only
//! says that the profile shows the app: whether it is installed, and where,
//! is Steam's to say, read from Steam's own files every time the library is
//! listed, so an entry outlives an uninstall in Steam and whatever lists
//! the apps a user owns can feed the same entries.

use std::collections::HashSet;
use std::path::PathBuf;

use decanter_core::{InstalledGame, SteamMark, Storefront};
use decanter_gog::LibraryEntry;

use super::{AppId, SteamGame};
use crate::i18n::Text;

/// The app id behind a Steam entry. A manifest is a file users can edit, so
/// its id is checked again where it is read back.
pub fn app_id(game: &InstalledGame) -> Option<AppId> {
    (game.storefront == Storefront::Steam)
        .then(|| game.game_id.parse().ok())
        .flatten()
}

/// Every Steam entry in `profile_id`, by name. An unreadable library reads
/// as none, the same as [`crate::external::list`].
pub fn list(profile_id: &str) -> Vec<InstalledGame> {
    let Ok(library) = crate::core::library_for_profile(profile_id) else {
        tracing::debug!(profile_id, "reporting no steam entries, library unavailable");
        return Vec::new();
    };
    let mut games: Vec<InstalledGame> = library
        .scan(profile_id)
        .into_values()
        .filter(|game| {
            let valid = app_id(game).is_some();
            if game.storefront == Storefront::Steam && !valid {
                tracing::warn!(profile_id, game_id = %game.game_id, "skipping steam entry with an invalid app id");
            }
            valid
        })
        .collect();
    games.sort_by_cached_key(|game| game.name.to_lowercase());
    games
}

/// The library rows for `profile_id`'s Steam entries, installed or not as
/// Steam has them right now.
pub fn rows(profile_id: &str) -> Vec<LibraryEntry> {
    let games = list(profile_id);
    if games.is_empty() {
        return Vec::new();
    }
    let installed = super::installed_apps();
    games
        .iter()
        .map(|game| {
            let is_installed = app_id(game).is_some_and(|id| installed.contains_key(&id));
            let mut entry = crate::external::entry(game);
            entry.installable = false;
            if let Some(state) = entry.game.install_state.as_mut() {
                state.installed = is_installed;
            }
            entry
        })
        .collect()
}

/// Where Steam has `game` installed, if it has it installed.
pub fn install_folder(game: &InstalledGame) -> Option<PathBuf> {
    let id = app_id(game)?;
    super::installed_apps().remove(&id)
}

/// The app ids of `profile_id`'s Steam entries, which the "Found in Steam"
/// list no longer offers.
pub fn added(profile_id: &str) -> HashSet<AppId> {
    list(profile_id).iter().filter_map(app_id).collect()
}

/// Files `found`, a Steam app, as a Steam entry of `profile_id`, which
/// finding Steam's games may add again from then on if it is removed.
pub fn add(profile_id: &str, found: &SteamGame) -> Result<(), Text> {
    let Some(id) = found.app_id else {
        return Err(Text::new("external.errors.save").arg("error", "not a steam app"));
    };
    let library = crate::achv_try!(crate::core::library_for_profile(profile_id));
    let game_id = id.to_string();
    let game = InstalledGame {
        account_id: profile_id.to_owned(),
        storefront: Storefront::Steam,
        // Never used as a directory, since Steam keeps the files, but the
        // manifest format needs a single path component here.
        install_dir: game_id.clone(),
        game_id,
        name: found.name.clone(),
        build_id: String::new(),
        size_on_disk: None,
        size_fingerprint: None,
        launch_executable: None,
        last_played: None,
        proton: None,
        branch: None,
        language: None,
        dlcs: Default::default(),
        cloud_save_path: None,
        cloud_save_last_sync: None,
        umu_id: None,
        umu_store: None,
        prefix_of: None,
        art: found.art.clone(),
    };
    crate::achv_try!(
        library
            .write_manifest(&game)
            .inspect(|()| tracing::info!(profile_id, app_id = %id, name = %game.name, "steam entry added"))
            .inspect_err(|err| tracing::error!(profile_id, app_id = %id, %err, "failed to add steam entry"))
            .map_err(|err| Text::new("external.errors.save").arg("error", err))
    );
    remember_dismissal(profile_id, &game.game_id, false);
    Ok(())
}

/// Files every game Steam has fully installed that `profile_id` shows no entry for yet, except the
/// ones the user removed, and returns how many it added. Blocking: reads Steam's manifests.
pub fn add_installed(profile_id: &str) -> usize {
    let Some(root) = super::root() else {
        return 0;
    };
    let dismissed = crate::core::db()
        .and_then(|db| {
            db.steam_marked(profile_id, SteamMark::RemovedApp)
                .map_err(crate::core::StoreError::Query)
        })
        .inspect_err(|err| tracing::warn!(profile_id, %err, "could not read removed steam entries, adding none"));
    let Ok(dismissed) = dismissed else {
        return 0;
    };
    let present = added(profile_id);
    let (mut count, mut failed, mut skipped) = (0_usize, 0_usize, 0_usize);
    for (app_id, app) in super::installed(&root) {
        if present.contains(&app_id) {
            continue;
        }
        if dismissed.contains(&app_id.to_string()) {
            skipped = skipped.saturating_add(1);
            continue;
        }
        let found = SteamGame {
            app_id: Some(app_id),
            name: app.name,
            folder: app.folder,
            executable: None,
            art: super::game_art(app.art),
        };
        // `add` has logged why one failed.
        if add(profile_id, &found).is_ok() {
            count = count.saturating_add(1);
        } else {
            failed = failed.saturating_add(1);
        }
    }
    if count > 0 || failed > 0 {
        tracing::info!(
            profile_id,
            added = count,
            failed,
            removed_by_user = skipped,
            "added steam's installed games"
        );
    } else {
        tracing::debug!(profile_id, removed_by_user = skipped, "no new steam games to add");
    }
    count
}

fn remember_dismissal(profile_id: &str, game_id: &str, dismissed: bool) {
    let saved = crate::core::db().and_then(|db| {
        db.set_steam_marked(profile_id, SteamMark::RemovedApp, game_id, dismissed)
            .map_err(crate::core::StoreError::Query)
    });
    if let Err(err) = saved {
        tracing::warn!(profile_id, game_id, dismissed, %err, "could not record whether the steam entry was removed");
    }
}

/// Forgets a Steam entry (manifest, favorites, playtime, collections).
/// Nothing of Steam's is touched: the game stays installed there. Remembered,
/// so that finding Steam's games doesn't add it back.
pub fn remove(profile_id: &str, game_id: &str) -> Result<(), Text> {
    let library = crate::achv_try!(crate::core::library_for_profile(profile_id));
    crate::achv_try!(
        library
            .remove_manifest(profile_id, Storefront::Steam, game_id)
            .inspect_err(|err| tracing::error!(profile_id, game_id, %err, "failed to remove steam entry manifest"))
            .map_err(|err| Text::new("external.errors.remove").arg("error", err))
    );
    // Best-effort, as for an external game: rows for an id nothing shows
    // any more are only clutter.
    if let Err(err) = crate::core::db().and_then(|db| {
        db.forget_product(profile_id, Storefront::Steam, game_id)
            .map_err(crate::core::StoreError::Query)
    }) {
        tracing::warn!(profile_id, game_id, %err, "could not forget steam entry's favorites, playtime and collections");
    }
    remember_dismissal(profile_id, game_id, true);
    tracing::info!(profile_id, game_id, "steam entry removed");
    Ok(())
}
