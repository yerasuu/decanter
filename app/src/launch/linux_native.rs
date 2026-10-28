//! External games that are Linux programs, run as themselves with their
//! prefix's path only naming their session.

use std::path::PathBuf;

use decanter_core::InstalledGame;
use decanter_runtime::Launch;

use super::Planned;

/// What the runtime reports a Linux program's launch as, since it starts
/// through the same backend as `umu-run`.
const LAUNCHER: &str = "umu-run";

pub(super) fn plan(account_id: &str, game: &InstalledGame, executable: PathBuf) -> Option<Planned> {
    let Some(prefix) = crate::core::prefix_for(account_id, &game.game_id) else {
        tracing::error!(game_id = %game.game_id, "no prefix path");
        return None;
    };
    // Under its umu id although nothing reads it but the logs, so a game's
    // lines read the same whichever way it runs.
    let mut launch = super::umu::identified(
        Launch::new(executable, super::session_key(account_id, &game.game_id)),
        game,
    );
    launch.native = true;
    let mangohud = super::session_tools(account_id, game, &mut launch);
    let facts = vec![
        ("game", game.name.clone()),
        ("game_id", game.game_id.clone()),
        ("runtime", "native Linux".to_owned()),
    ];
    launch.prefix = Some(prefix);
    Some(Planned {
        launch,
        launcher: LAUNCHER,
        achievement_log: None,
        facts,
        mangohud,
    })
}
