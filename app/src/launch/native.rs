//! Games run as the Windows programs they are: their own executable,
//! started directly from its folder.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{OnceLock, PoisonError, RwLock};
use std::time::SystemTime;

use decanter_core::InstalledGame;
use decanter_ps::process::Job;
use decanter_ps::run_log::RunLog;
use decanter_runtime::{Launch, Prepared};
use gpui::{AsyncApp, Entity};
use rust_i18n::t;

/// What a run that never started couldn't start, for its log.
pub(super) const LAUNCHER: &str = "the game";

/// The imports through which a game reaches `DirectX` 7: `DirectDraw` and
/// `Direct3D`'s immediate mode.
const DX7_IMPORTS: [&str; 3] = ["ddraw.dll", "d3dim.dll", "d3dim700.dll"];

const DX7_HINT: &str = "# hint: the game imports DirectX 7 (DirectDraw) and ended with a failure status; \
                        it may need a DirectDraw wrapper (such as cnc-ddraw or dgVoodoo2) beside its executable";

/// An executable as it was when its imports were read: a game updated in
/// place is read again.
type ImportsKey = (PathBuf, u64, Option<SystemTime>);

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

/// Points at a `DirectDraw` wrapper when a game importing `DirectX` 7 ends with
/// a failure status, `log` handed back for the run to finish. Only then: one
/// that runs is left alone, and decanter never puts a DLL beside a game
/// itself (decision #516).
pub(super) async fn hint_after_failure(
    executable: &Path,
    exit: Option<ExitStatus>,
    job: &Entity<Job>,
    log: Option<RunLog>,
    game_id: &str,
    cx: &mut AsyncApp,
) -> Option<RunLog> {
    let Some(status) = exit.filter(|status| !status.success()) else {
        return log;
    };
    if job.read_with(cx, |job, _| job.stop_requested) || !imports_dx7(executable).await {
        return log;
    }
    tracing::warn!(
        game_id,
        exit_code = status
            .code()
            .map(i32::cast_unsigned)
            .map(|code| format!("{code:#010x}")),
        "game importing DirectX 7 ended with a failure status"
    );
    crate::run_log::note(job, log, DX7_HINT, t!("library.run.dx7_hint").into_owned(), game_id, cx).await
}

/// Whether `executable` imports one of [`DX7_IMPORTS`], read once per
/// version of the file.
async fn imports_dx7(executable: &Path) -> bool {
    let path = executable.to_path_buf();
    let read = tokio::task::spawn_blocking(move || {
        let metadata = std::fs::metadata(&path)?;
        let key = (path, metadata.len(), metadata.modified().ok());
        if let Some(&known) = imports_cache().read().unwrap_or_else(PoisonError::into_inner).get(&key) {
            return Ok(known);
        }
        let imports = match super::pe::imported_dlls(&key.0) {
            Ok(dlls) => dlls
                .iter()
                .any(|dll| DX7_IMPORTS.iter().any(|dx7| dll.eq_ignore_ascii_case(dx7))),
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                tracing::debug!(path = %key.0.display(), %err, "launch executable is not a readable PE image");
                false
            }
            Err(err) => return Err(err),
        };
        imports_cache()
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, imports);
        Ok(imports)
    })
    .await;
    match read {
        Ok(Ok(imports)) => imports,
        Ok(Err(err)) => {
            tracing::warn!(path = %executable.display(), %err, "could not read the executable's imports");
            false
        }
        Err(err) => {
            tracing::warn!(path = %executable.display(), %err, "reading the executable's imports did not finish");
            false
        }
    }
}

fn imports_cache() -> &'static RwLock<HashMap<ImportsKey, bool>> {
    static IMPORTS: OnceLock<RwLock<HashMap<ImportsKey, bool>>> = OnceLock::new();
    IMPORTS.get_or_init(Default::default)
}
