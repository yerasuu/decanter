//! Games run under Proton through umu-run, each in its own prefix — and
//! external games that are Linux programs, run as themselves with the
//! prefix's path only naming their session.

use std::path::PathBuf;

use decanter_core::InstalledGame;
use decanter_runtime::{Launch, Prepared};

use crate::i18n::Text;

/// What a run that never started couldn't start, for its log.
pub(super) const LAUNCHER: &str = "umu-run";

/// A launch resolved before anything is synced or spawned, so a game with
/// no prefix to run in fails before its cloud saves are touched.
pub(super) struct Planned {
    launch: Launch,
    /// Where decanter-hook writes this session's unlocks; GOG games only.
    pub(super) achievement_log: Option<PathBuf>,
    /// Header lines for the run's log.
    pub(super) facts: Vec<(&'static str, String)>,
    /// The game's key, its library's root and whether MangoHud logs from
    /// the start, when it runs with MangoHud.
    mangohud: Option<(String, PathBuf, bool)>,
}

pub(super) fn plan(account_id: &str, game: &InstalledGame, executable: PathBuf) -> Option<Planned> {
    let Some(prefix) = crate::core::prefix_for(account_id, &game.game_id) else {
        tracing::error!(game_id = %game.game_id, "no prefix path");
        return None;
    };

    // Read from the file rather than kept in the manifest: what decides is
    // what the executable is now, and only an external game can be a Linux
    // program.
    let native = game.storefront == decanter_core::Storefront::Unspecified
        && crate::external::probe::is_native_linux(&executable);

    // `umu-<product id>` with `STORE=gog` is how protonfixes finds a GOG
    // game's workarounds. An external game's uuid is in no database, so it
    // goes in as `umu-default` with no store instead — the same as Heroic
    // runs a sideloaded game — which some Proton builds (DW-Proton) map to
    // the right id from the executable; a made-up id skipped that, and with
    // it fixes a game like Arknights: Endfield can't start without.
    // One picked from the UMU database by name goes in as that entry, store
    // and all, which works with any Proton carrying protonfixes.
    let launch = Launch::new(executable, format!("{account_id}/{}", game.game_id));
    let mut launch = match (game.storefront, &game.umu_id) {
        (decanter_core::Storefront::Gog, _) => launch.game_id(format!("umu-{}", game.game_id)).store("gog"),
        (_, Some(id)) => {
            let launch = launch.game_id(id.as_str());
            match &game.umu_store {
                Some(store) => launch.store(store.as_str()),
                None => launch,
            }
        }
        (_, None) => launch.game_id(crate::external::UMU_ID),
    };
    launch.native = native;
    launch.proton = crate::core::proton_for(game);
    let key = crate::taskbar::key(game.storefront, &game.game_id);
    let (mangohud, gamemoded) = crate::core::db()
        .map(|db| {
            (
                crate::game_tools::mangohud(&db, &key),
                crate::game_tools::gamemoded(&db, &key),
            )
        })
        .unwrap_or_default();
    launch.gamemoded = gamemoded;
    let mangohud = mangohud.and_then(|autostart_log| match crate::core::library_for_account(account_id) {
        Ok(library) => Some((key.clone(), library.root().to_path_buf(), autostart_log)),
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "no library to log MangoHud to, starting without it");
            None
        }
    });
    launch.identity = Some((key, game.name.clone()));
    launch.capture_output = true;
    launch.env.extend(crate::cache::launch_env(game));

    // decanter-hook (a GOG-only concern -- a shortcut has no Galaxy DLL
    // to patch) reads both of these itself; setting them here is what
    // lets it write somewhere decanter actually knows to look afterward,
    // and tag what it wrote with the right product id.
    let achievement_log = (game.storefront == decanter_core::Storefront::Gog).then(|| {
        let path = decanter_gog::hook::achievement_log_path(&prefix);
        launch
            .env
            .push(("DECANTER_PRODUCT_ID".to_owned(), game.game_id.clone()));
        launch
            .env
            .push(("DECANTER_ACHIEVEMENT_LOG".to_owned(), path.display().to_string()));
        path
    });

    let facts = vec![
        ("game", game.name.clone()),
        ("game_id", game.game_id.clone()),
        if native {
            ("runtime", "native Linux".to_owned())
        } else {
            ("prefix", prefix.path().display().to_string())
        },
    ];
    launch.prefix = Some(prefix);
    Some(Planned {
        launch,
        achievement_log,
        facts,
        mangohud,
    })
}

impl Planned {
    pub(super) async fn prepare(mut self) -> anyhow::Result<Prepared> {
        if let Some((key, library_root, autostart_log)) = self.mangohud.take() {
            self.launch.env.extend(
                crate::game_tools::mangohud_env(&key, &library_root, autostart_log)
                    .await
                    .map(|(name, value)| (name.to_owned(), value)),
            );
        }
        decanter_runtime::prepare(self.launch).await
    }
}

/// Prefix-wide, which is now the same thing as game-wide: each game has
/// its own prefix, so stopping one can't reach another.
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
