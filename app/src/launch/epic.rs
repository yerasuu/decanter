//! What an Epic Games title gets before it starts under Proton (stage E6 of
//! docs/epic-legendary-integration-analysis.md): the offline command line of spec section 8.2,
//! offline by decision D10 for every title, and the umu id the UMU database files its `appName`
//! under. No exchange code, no ownership token and no EOS overlay (D11), so nothing secret reaches
//! the game; no achievement hook and no cloud saves either.

use decanter_core::{CachedGame, InstalledGame, Storefront};
use decanter_epic::OfflineLaunch;
use decanter_runtime::Launch;
use rust_i18n::t;

/// The launcher's own fallback when no language is known (spec, 8.2).
const DEFAULT_LANGUAGE: &str = "en";

/// Sets `launch` up for `game`; what the run's log should say, as lines. Nothing here stops the
/// launch: a value that is missing is logged with what was sent instead.
pub(super) async fn prepare(account_id: &str, game: &InstalledGame, launch: &mut Launch) -> Vec<String> {
    let app_name = game.game_id.as_str();
    identify(game, launch).await;
    let cached = crate::views::library::epic_install::cached_game(account_id, app_name);
    let mut notes = Vec::new();
    let (can_run_offline, additional_command_line) = attributes(account_id, app_name, cached.as_ref());
    if !can_run_offline {
        notes.push(t!("epic.launch.online_needed").into_owned());
    }
    let launch_command = match launch_command(account_id, app_name).await {
        Ok(command) => command,
        Err(reason) => {
            tracing::warn!(
                account_id,
                app_name,
                reason,
                "epic game starts without its manifest's arguments"
            );
            notes.push(t!("epic.launch.command_failed", reason = reason).into_owned());
            String::new()
        }
    };
    let namespace = cached.and_then(|cached| cached.namespace).unwrap_or_else(|| {
        tracing::warn!(
            account_id,
            app_name,
            "no cached epic namespace, -epicsandboxid left empty"
        );
        String::new()
    });
    let display_name = display_name(account_id);
    let language = language(account_id, app_name);
    let offline = OfflineLaunch::new(app_name, &namespace, account_id, &display_name, &language);
    launch.args.extend(decanter_epic::offline_arguments(
        &offline,
        &launch_command,
        &additional_command_line,
    ));
    tracing::info!(
        account_id,
        app_name,
        language,
        game_arguments = launch_command.split_whitespace().count(),
        additional_arguments = additional_command_line.split_whitespace().count(),
        "epic game starts offline"
    );
    notes
}

/// Puts the game under the database row its `appName` is the codename of, unless one was picked
/// for it in its properties; without a row it keeps `umu-default` under the `egs` store.
async fn identify(game: &InstalledGame, launch: &mut Launch) {
    if game.umu_id.is_some() {
        return;
    }
    let app_name = game.game_id.as_str();
    if let Some(id) = crate::external::umu::id_for_codename(super::umu::EPIC_STORE, app_name).await {
        tracing::info!(
            app_name,
            umu_id = id,
            "epic game found in the umu database by its app name"
        );
        launch.game_id = id;
    } else {
        tracing::info!(
            app_name,
            umu_id = %launch.game_id,
            "epic game not in the installed umu database, starting as umu-default under the egs store"
        );
    }
}

/// `CanRunOffline` and the additional command line, as the library listing cached them; the spec's
/// defaults (offline, none) for a game it has not described yet.
fn attributes(account_id: &str, app_name: &str, cached: Option<&CachedGame>) -> (bool, String) {
    let Some(cached) = cached else {
        tracing::warn!(
            account_id,
            app_name,
            "epic game not in the cached library, starting it as able to run offline with no additional arguments"
        );
        return (true, String::new());
    };
    if cached.ownership_token == Some(true) {
        tracing::info!(
            account_id,
            app_name,
            "the epic catalog asks for an ownership token, which only an online launch hands over"
        );
    }
    let can_run_offline = match cached.can_run_offline {
        Some(true) => true,
        Some(false) => {
            tracing::warn!(
                account_id,
                app_name,
                "the epic catalog says the game needs the online session, starting it offline anyway"
            );
            false
        }
        None => {
            tracing::info!(
                account_id,
                app_name,
                "CanRunOffline not cached yet, taken as true as the launcher does"
            );
            true
        }
    };
    let additional_command_line = cached.additional_command_line.clone().unwrap_or_else(|| {
        tracing::info!(
            account_id,
            app_name,
            "additional command line not cached yet, none added"
        );
        String::new()
    });
    (can_run_offline, additional_command_line)
}

/// The `launch_command` of the manifest the game was installed from.
async fn launch_command(account_id: &str, app_name: &str) -> Result<String, String> {
    let library = crate::core::library_for_account(account_id).map_err(|err| err.to_string())?;
    match decanter_epic::installed_manifest(&library, account_id, app_name).await {
        Ok(Some(manifest)) => Ok(manifest.launch_command().to_owned()),
        Ok(None) => Err("the game has no install record".to_owned()),
        Err(err) => Err(format!("{err:#}")),
    }
}

/// The name Epic gave the account at login; its id when that can't be read, since the launcher
/// always sends one.
fn display_name(account_id: &str) -> String {
    let stored = crate::core::db().map_err(|err| err.to_string()).and_then(|db| {
        db.account_name(account_id, Storefront::EpicGames)
            .map_err(|err| err.to_string())
    });
    match stored {
        Ok(Some(name)) => name,
        Ok(None) => {
            tracing::warn!(
                account_id,
                "no stored epic account name, -epicusername takes the account id"
            );
            account_id.to_owned()
        }
        Err(err) => {
            tracing::warn!(account_id, %err, "epic account name unreadable, -epicusername takes the account id");
            account_id.to_owned()
        }
    }
}

/// The game's own language when one was picked for it, else the app's, as a bare language code.
fn language(account_id: &str, app_name: &str) -> String {
    let picked = crate::core::db().ok().and_then(|db| {
        db.game_language(account_id, Storefront::EpicGames, app_name)
            .inspect_err(|err| tracing::warn!(account_id, app_name, %err, "epic game language unreadable"))
            .ok()
            .flatten()
    });
    let tag = picked.unwrap_or_else(|| rust_i18n::locale().to_string());
    let language = tag.split(['-', '_']).next().unwrap_or_default();
    if (2..=3).contains(&language.len()) && language.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        language.to_ascii_lowercase()
    } else {
        tracing::warn!(
            account_id,
            app_name,
            tag,
            "no language code in the tag, -epiclocale takes en"
        );
        DEFAULT_LANGUAGE.to_owned()
    }
}
