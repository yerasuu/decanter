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

use decanter_core::{InstalledGame, Storefront};
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

/// Files `found`, a Steam app, as a Steam entry of `profile_id`.
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
    library
        .write_manifest(&game)
        .inspect(|()| tracing::info!(profile_id, app_id = %id, name = %game.name, "steam entry added"))
        .inspect_err(|err| tracing::error!(profile_id, app_id = %id, %err, "failed to add steam entry"))
        .map_err(|err| Text::new("external.errors.save").arg("error", err))
}

/// Forgets a Steam entry (manifest, favorites, playtime, collections).
/// Nothing of Steam's is touched: the game stays installed there.
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
    tracing::info!(profile_id, game_id, "steam entry removed");
    Ok(())
}
