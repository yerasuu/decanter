//! Playing on Windows: the `psport.exe` of the folder picked in the section's Ajustes, else of the
//! Windows package in use ([`super::package`]), started directly in a session of its own (a Job
//! Object), with no toolchain, bash or `run.sh`.

use std::path::{Path, PathBuf};

use decanter_ps::bbport::Bbport;
use decanter_ps::bbport::direct::{FOLDER_KEY, Folder};
use decanter_ps::library::Game;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_runtime::Launch;
use gpui::{AppContext as _, AsyncApp, Context, WeakEntity};

use super::Ps;
use super::jobs::{JobKey, Launcher};
use super::play::launch_inputs;
use crate::i18n::Text;

/// What a direct launch is for: the game, whose profile plays it, and where its caches go.
pub(super) struct Request<'launch> {
    pub(super) root: &'launch Root,
    pub(super) port: Port,
    pub(super) game: &'launch Game,
    pub(super) profile_id: &'launch str,
    pub(super) cache_dir: Option<&'launch Path>,
}

/// `psport.exe` with the game's environment and `run.sh`'s arguments, ready to spawn, and the
/// facts for its log's header. `this` hears of a first download of the package.
pub(super) async fn launch(
    this: &WeakEntity<Ps>,
    request: Request<'_>,
    cx: &mut AsyncApp,
) -> Result<(Launcher, Vec<(&'static str, String)>), Text> {
    let Request {
        root,
        port,
        game,
        profile_id,
        cache_dir,
    } = request;
    // AnyPS5 converts a game with its own tools first, which only its toolchain has.
    if port != Port::Bbport {
        tracing::warn!(port = port.name(), title_id = %game.title_id, "this port does not run without its toolchain here");
        return Err(Text::new("ps.play.direct.port_unavailable").arg("name", port.name()));
    }
    let inputs = match launch_inputs(root, None, game, profile_id, cx).await {
        Ok(inputs) => inputs,
        Err(err) => {
            tracing::warn!(title_id = %game.title_id, error = %err, "could not start game");
            return Err(err.localized());
        }
    };
    let dir = match inputs.settings.text(FOLDER_KEY) {
        Some(dir) => PathBuf::from(dir),
        None => match super::package::for_play(this, root, &inputs.settings, cx).await {
            Ok(dir) => dir,
            Err(err) => return Err(err),
        },
    };
    // Checked again at each launch: the folder may have been moved or emptied since it was picked.
    let folder = match cx.background_spawn(async move { Folder::try_new(dir) }).await {
        Ok(folder) => folder,
        Err(err) => {
            tracing::warn!(error = %err, "the psport.exe folder is no longer usable");
            return Err(Text::new("ps.play.direct.bad_folder").text("error", super::folder_error(&err)));
        }
    };
    let mut launch = Launch::new(folder.executable(), session_key(&game.dir));
    launch.capture_output = true;
    let mut prepared = match decanter_runtime::prepare(launch) {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::warn!(executable = %folder.executable().display(), error = %format_args!("{err:#}"), "could not prepare psport.exe");
            return Err(Text::new("ps.play.prepare_error")
                .arg("name", port.name())
                .arg("error", format_args!("{err:#}")));
        }
    };
    let built = Bbport::new(root.clone())
        .direct_command(prepared.command_mut(), &folder, &inputs.game_launch(game, cache_dir))
        .await;
    if let Err(err) = built {
        tracing::warn!(title_id = %game.title_id, %err, "could not set up psport.exe's launch");
        return Err(Text::new("ps.errors.environment").arg("error", err));
    }
    let facts = vec![
        ("port", port.name().to_owned()),
        ("psport_folder", folder.path().display().to_string()),
    ];
    Ok((Launcher::Session(prepared), facts))
}

/// How [`stop`] ends a game's session.
#[derive(Clone, Copy)]
pub(super) enum Ending {
    /// Asked to close first, so the game can save, and ended if it won't.
    Polite,
    /// Ended at once.
    Now,
}

/// Stops the game's session unless a stop of it is already under way; the exit it causes reads as a
/// stop.
pub(super) fn stop(ps: &mut Ps, dir: &Path, ending: Ending, cx: &mut Context<Ps>) {
    let Some(job) = ps.jobs.get(&JobKey::Game(dir.to_owned())) else {
        return;
    };
    if !job.read(cx).is_running() || !ps.stopping.insert(dir.to_owned()) {
        return;
    }
    job.update(cx, |job, _| job.stop_requested = true);
    cx.notify();
    let key = session_key(dir);
    let dir = dir.to_owned();
    cx.spawn(async move |this, cx| {
        match ending {
            Ending::Polite => decanter_runtime::stop(&key).await,
            Ending::Now => decanter_runtime::end(&key).await,
        }
        let _ = this.update(cx, |this, cx| {
            this.stopping.remove(&dir);
            cx.notify();
        });
    })
    .detach();
}

/// One session per dump folder, as one job per game.
fn session_key(dir: &Path) -> String {
    format!("ps:{}", dir.display())
}
