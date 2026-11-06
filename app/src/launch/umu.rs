//! Windows games run under Proton through umu-run, each in its own prefix.

use std::path::PathBuf;
use std::time::Duration;

use decanter_core::InstalledGame;
use decanter_ps::process::Job;
use decanter_ps::run_log::RunLog;
use decanter_runtime::Launch;
use gpui::{AsyncApp, Entity};
use rust_i18n::t;

use super::Planned;
use crate::i18n::Text;

const LAUNCHER: &str = "umu-run";

/// Longer than any game takes to give up on a screen mode it cannot set, so a session that ran
/// for a while is never blamed on it.
const MODE_FAILURE_WINDOW: Duration = Duration::from_secs(30);

const WAYLAND_MODE_HINT: &str = "# hint: the game asked for a screen mode that the Wayland window driver \
                                 (winewayland.drv) does not offer and ended within seconds; \
                                 a Proton that uses X11 offers the mode";

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

/// Installs in `launch`'s prefix the redistributables `game`'s build named
/// that haven't run there yet; what failed, as lines for the run's log.
pub(super) async fn install_redist(account_id: &str, game: &InstalledGame, launch: &Launch) -> Vec<String> {
    let Some(prefix) = launch.prefix.as_ref() else {
        return Vec::new();
    };
    let root = match crate::core::library_for_account(account_id) {
        Ok(library) => library.root_of(account_id, &game.game_id),
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "no library to find the redistributables in, starting without them");
            None
        }
    };
    let Some(root) = root else {
        return Vec::new();
    };
    decanter_gog::run_redist(&root, &game.game_id, prefix, &launch.proton)
        .await
        .into_iter()
        .map(|failure| t!("library.redist.failed", name = failure.name, reason = failure.reason).into_owned())
        .collect()
}

/// Registers the stand-in Galaxy service in `launch`'s prefix when `game` ships the Galaxy SDK;
/// a failure is logged there and never stops the launch.
pub(super) async fn register_galaxy_service(account_id: &str, game: &InstalledGame, launch: &Launch) {
    let Some(prefix) = launch.prefix.as_ref() else {
        return;
    };
    let install_dir = match crate::core::library_for_account(account_id) {
        Ok(library) => library.install_dir(&game.install_dir),
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "no library to find the install in, no Galaxy service");
            None
        }
    };
    let Some(install_dir) = install_dir else {
        return;
    };
    decanter_gog::galaxy_service::ensure(&install_dir, &launch.executable, prefix, &launch.proton).await;
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
/// An Amazon game goes in as an external one: protonfixes has an `amazon`
/// store, but its database keys those rows by Steam-derived `umu-<app id>`
/// ids rather than Amazon's product id, and GE-Proton11-9 ships no fix
/// module for that store, so `umu-<product id>` with `STORE=amazon` would
/// match nothing and lose the default fixes `umu-default` gets.
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

/// Points at the Wayland window driver when a short session shows it refused the game's screen
/// mode: `winewayland.drv` lists only each output's own modes, so a game that insists on another
/// resolution (640x480 fullscreen, say) gets `DISP_CHANGE_BADMODE` and Direct3D never starts.
/// Read from the run's Wine log, so it holds for any game; `log` is handed back for the run to
/// finish.
pub(super) async fn hint_after_mode_failure(
    job: &Entity<Job>,
    log: Option<RunLog>,
    elapsed: Duration,
    game_id: &str,
    cx: &mut AsyncApp,
) -> Option<RunLog> {
    if elapsed > MODE_FAILURE_WINDOW {
        return log;
    }
    let refused = job.read_with(cx, |job, _| {
        !job.stop_requested && refused_by_wayland_driver(job.lines.iter().map(String::as_str))
    });
    if !refused {
        return log;
    }
    tracing::warn!(
        game_id,
        seconds = elapsed.as_secs(),
        "game ended soon after winewayland refused its screen mode"
    );
    crate::run_log::note(
        job,
        log,
        WAYLAND_MODE_HINT,
        t!("library.run.wayland_mode_hint").into_owned(),
        game_id,
        cx,
    )
    .await
}

/// `-2` is `DISP_CHANGE_BADMODE`, what `NtUserChangeDisplaySettings` answers to a mode the driver
/// does not list.
fn refused_by_wayland_driver<'line>(lines: impl Iterator<Item = &'line str>) -> bool {
    let mut is_wayland = false;
    let mut is_refused = false;
    for line in lines {
        is_wayland |= line.contains("Loaded") && line.contains("winewayland.drv");
        is_refused |= line.contains("NtUserChangeDisplaySettings") && line.contains("returned -2.");
        if is_wayland && is_refused {
            return true;
        }
    }
    false
}
