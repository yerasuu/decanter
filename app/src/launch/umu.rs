//! Windows games run under Proton through umu-run, each in its own prefix.

use std::path::PathBuf;

use decanter_core::InstalledGame;
use decanter_runtime::Launch;

use super::Planned;
use crate::i18n::Text;

const LAUNCHER: &str = "umu-run";

pub(super) fn plan(account_id: &str, game: &InstalledGame, executable: PathBuf) -> Option<Planned> {
    let Some(prefix) = crate::core::prefix_for(account_id, &game.game_id) else {
        tracing::error!(game_id = %game.game_id, "no prefix path");
        return None;
    };
    let mut launch = identified(
        Launch::new(executable, super::session_key(account_id, &game.game_id)),
        game,
    );
    launch.proton = crate::core::proton_for(game);
    let mangohud = super::session_tools(account_id, game, &mut launch);

    // decanter-hook (a GOG-only concern -- a shortcut has no Galaxy DLL
    // to patch) reads both of these itself.
    let achievement_log = (game.storefront == decanter_core::Storefront::Gog).then(|| {
        let path = decanter_gog::hook::achievement_log_in_prefix(&prefix);
        super::hook_env(&mut launch, &game.game_id, &path);
        path
    });

    let facts = vec![
        ("game", game.name.clone()),
        ("game_id", game.game_id.clone()),
        ("prefix", prefix.path().display().to_string()),
    ];
    launch.prefix = Some(prefix);
    Some(Planned {
        launch,
        launcher: LAUNCHER,
        achievement_log,
        facts,
        mangohud,
        is_raven: false,
    })
}

/// `launch` under the umu id protonfixes looks `game` up by.
///
/// `umu-<product id>` with `STORE=gog` is how protonfixes finds a GOG
/// game's workarounds. An external game's uuid is in no database, so it
/// goes in as `umu-default` with no store instead — the same as Heroic
/// runs a sideloaded game — which some Proton builds (DW-Proton) map to
/// the right id from the executable; a made-up id skipped that, and with
/// it fixes a game like Arknights: Endfield can't start without.
/// One picked from the UMU database by name goes in as that entry, store
/// and all, which works with any Proton carrying protonfixes.
pub(super) fn identified(launch: Launch, game: &InstalledGame) -> Launch {
    match (game.storefront, &game.umu_id) {
        (decanter_core::Storefront::Gog, _) => launch.game_id(format!("umu-{}", game.game_id)).store("gog"),
        (_, Some(id)) => {
            let launch = launch.game_id(id.as_str());
            match &game.umu_store {
                Some(store) => launch.store(store.as_str()),
                None => launch,
            }
        }
        (_, None) => launch.game_id(crate::external::UMU_ID),
    }
}

/// Prefix-wide, which is now the same thing as game-wide: each game has
/// its own prefix, so stopping one can't reach another. Unlike a stop by
/// session key, it also reaches a game left running by an earlier run of
/// decanter.
pub(super) async fn stop(account_id: &str, game_id: &str) {
    let Some(prefix) = crate::core::prefix_for(account_id, game_id) else {
        tracing::warn!(account_id, game_id, "cannot stop, no prefix resolved");
        return;
    };
    match prefix.stop().await {
        Ok(()) => tracing::info!(account_id, game_id, "game stopped"),
        Err(err) => tracing::error!(account_id, game_id, error = %format_args!("{err:#}"), "stop failed"),
    }
}

pub(super) fn is_running(account_id: &str, game_id: &str) -> bool {
    crate::core::prefix_for(account_id, game_id).is_some_and(|prefix| !prefix.processes().is_empty())
}

/// Throws a game's prefix away so umu builds a fresh one next launch.
///
/// The escape hatch for a prefix a game has wrecked — the reason each
/// game gets its own. Saves a game keeps inside its prefix go with it,
/// which is why this is never automatic.
pub async fn reset_prefix(account_id: String, game_id: String) -> Result<(), Text> {
    let Some(prefix) = crate::core::prefix_for(&account_id, &game_id) else {
        return Err(Text::new("messages.errors.no_prefix"));
    };
    // Nothing may be running in it: deleting a live prefix out from under
    // Wine corrupts whatever it was writing.
    crate::achv_try!(
        prefix
            .stop()
            .await
            .inspect_err(
                |err| tracing::error!(account_id, game_id, %err, "failed to stop game before resetting its prefix")
            )
            .map_err(Text::verbatim)
    );
    match tokio::fs::remove_dir_all(prefix.path()).await {
        Ok(()) => {
            tracing::info!(account_id, game_id, "prefix reset");
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(account_id, game_id, "prefix already absent, nothing to reset");
            Ok(())
        }
        Err(err) => {
            tracing::error!(account_id, game_id, %err, "failed to remove prefix directory");
            Err(Text::new("messages.errors.delete_prefix").arg("error", err))
        }
    }
}
