//! Playing on Windows: the `psport.exe` of the folder picked in the section's Ajustes, started
//! directly in a session of its own (a Job Object), with no toolchain, bash or `run.sh`.

use std::path::{Path, PathBuf};

use decanter_ps::bbport::Bbport;
use decanter_ps::bbport::direct::{FOLDER_KEY, Folder};
use decanter_ps::library::Game;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::Job;
use decanter_runtime::Launch;
use gpui::{AppContext as _, AsyncApp, Context, Entity};

use super::Ps;
use super::jobs::Launcher;
use super::play::launch_inputs;
use crate::i18n::Text;

/// `psport.exe` with the game's environment and `run.sh`'s arguments, ready to spawn, and the
/// facts for its log's header.
pub(super) async fn launch(
    root: &Root,
    port: Port,
    game: &Game,
    profile_id: &str,
    cache_dir: Option<&Path>,
    cx: &mut AsyncApp,
) -> Result<(Launcher, Vec<(&'static str, String)>), Text> {
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
    let Some(dir) = inputs.settings.text(FOLDER_KEY).map(PathBuf::from) else {
        tracing::info!(title_id = %game.title_id, "no psport.exe folder picked");
        return Err(Text::new("ps.play.direct.no_folder"));
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

/// Asks the game's session to close, then ends it if it won't; the exit it causes reads as a stop.
pub(super) fn stop(job: &Entity<Job>, dir: &Path, cx: &mut Context<Ps>) {
    if !job.read(cx).is_running() {
        return;
    }
    job.update(cx, |job, cx| {
        job.stop_requested = true;
        cx.notify();
    });
    let key = session_key(dir);
    cx.spawn(async move |_, _| decanter_runtime::stop(&key).await).detach();
}

/// One session per dump folder, as one job per game.
fn session_key(dir: &Path) -> String {
    format!("ps:{}", dir.display())
}
