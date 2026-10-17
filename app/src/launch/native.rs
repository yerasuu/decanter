//! Games run as the Windows programs they are: their own executable,
//! started directly from its folder.

use std::path::{Path, PathBuf};

use decanter_core::InstalledGame;
use decanter_runtime::{Launch, Prepared};

/// What a run that never started couldn't start, for its log.
pub(super) const LAUNCHER: &str = "the game";

/// A launch resolved before anything is synced or spawned.
pub(super) struct Planned {
    launch: Launch,
    /// Always `None`: decanter-hook isn't deployed here (see `plan`).
    pub(super) achievement_log: Option<PathBuf>,
    /// Header lines for the run's log.
    pub(super) facts: Vec<(&'static str, String)>,
}

pub(super) fn plan(account_id: &str, game: &InstalledGame, executable: PathBuf) -> Option<Planned> {
    if game.storefront == decanter_core::Storefront::Gog {
        // decanter-hook stands in for GOG Galaxy under Wine; here the game's
        // own Galaxy64.dll stays in place, so unlocks reach GOG only through
        // the Galaxy client, and decanter learns of them from the poll after
        // the session.
        tracing::warn!(game_id = %game.game_id, "achievement hook not deployed on this system, local unlocks are not recorded");
    }
    // The executable's own folder, which is where most games load their
    // data from by relative path.
    let Some(working_directory) = executable.parent().map(Path::to_path_buf) else {
        tracing::error!(game_id = %game.game_id, path = %executable.display(), "launch executable has no folder to run in");
        return None;
    };
    let facts = vec![
        ("game", game.name.clone()),
        ("game_id", game.game_id.clone()),
        ("working_directory", working_directory.display().to_string()),
    ];
    let mut launch = Launch::new(executable, session_key(account_id, &game.game_id));
    launch.working_directory = Some(working_directory);
    launch.capture_output = true;
    Some(Planned {
        launch,
        achievement_log: None,
        facts,
    })
}

impl Planned {
    /// A future only to share the Proton backend's shape, whose prepare
    /// awaits umu-run.
    pub(super) fn prepare(self) -> core::future::Ready<anyhow::Result<Prepared>> {
        core::future::ready(decanter_runtime::prepare(self.launch))
    }
}

pub(super) async fn stop(account_id: &str, game_id: &str) {
    let key = session_key(account_id, game_id);
    if !decanter_runtime::is_live(&key) {
        tracing::warn!(account_id, game_id, "cannot stop, no running session");
        return;
    }
    decanter_runtime::stop(&key).await;
    tracing::info!(account_id, game_id, "game stopped");
}

/// Whether `game_id` of `account_id` has a session decanter started and
/// hasn't seen exit.
pub fn is_running(account_id: &str, game_id: &str) -> bool {
    decanter_runtime::is_live(&session_key(account_id, game_id))
}

/// Per account, the same as a game's manifest: two profiles owning one
/// game run it as two sessions.
fn session_key(account_id: &str, game_id: &str) -> String {
    format!("{account_id}/{game_id}")
}
