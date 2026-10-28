//! Windows programs of the Raven section (#1067), run by Raven in a prefix of their own, which also
//! names their session: Raven itself starts as a Linux program, as [`super::linux_native`] does.

use std::path::PathBuf;

use decanter_core::InstalledGame;
use decanter_runtime::{Launch, Prefix};

use super::Planned;

const LAUNCHER: &str = "raven";

pub(super) fn plan(account_id: &str, game: &InstalledGame, executable: PathBuf) -> Option<Planned> {
    let Some(prefix) = crate::raven::prefix(&game.game_id).map(Prefix::new) else {
        tracing::error!(game_id = %game.game_id, "no Raven prefix path");
        return None;
    };
    let mut launch = Launch::new(executable, super::session_key(account_id, &game.game_id));
    let mangohud = super::session_tools(account_id, game, &mut launch);
    let facts = vec![
        ("program", game.name.clone()),
        ("program_id", game.game_id.clone()),
        ("runtime", format!("Raven {}", decanter_runtime::runner::RAVEN_COMMIT)),
        ("prefix", prefix.path().display().to_string()),
    ];
    launch.prefix = Some(prefix);
    Some(Planned {
        launch,
        launcher: LAUNCHER,
        achievement_log: None,
        facts,
        mangohud,
        is_raven: true,
    })
}

/// Everything running with the program's prefix as its session, Raven and what it started.
pub(super) async fn stop(game_id: &str) {
    let Some(prefix) = crate::raven::prefix(game_id).map(Prefix::new) else {
        tracing::warn!(game_id, "cannot stop, no Raven prefix");
        return;
    };
    match prefix.stop().await {
        Ok(()) => tracing::info!(game_id, "Raven program stopped"),
        Err(err) => tracing::error!(game_id, error = %format_args!("{err:#}"), "stop failed"),
    }
}

pub(super) fn is_running(game_id: &str) -> bool {
    crate::raven::prefix(game_id).is_some_and(|prefix| !Prefix::new(prefix).processes().is_empty())
}
