//! `psport.exe` started directly where decanter runs it so ([`decanter_ps::bbport::direct::IN_USE`]):
//! the folder picked in the section's Ajustes, else the Windows build or package in use, in a session
//! of its own (a Job Object), with no toolchain, bash or `run.sh`.

use std::path::{Path, PathBuf};

use decanter_ps::bbport::direct::{FOLDER_KEY, Folder};
use decanter_ps::bbport::{Bbport, LauncherSettings};
use decanter_ps::library::Game;
use decanter_ps::os::windows::package::{self, SELECTED_KEY, Version};
use decanter_ps::os::windows::winbuild::{self, Commit};
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_runtime::{Launch, Prepared};

use super::launch::LaunchInputs;
use crate::i18n::Text;

/// `AnyPS5` converts a game with its own tools first, which only its toolchain has.
pub fn check_port(port: Port, game: &Game) -> Result<(), Text> {
    if port == Port::Bbport {
        return Ok(());
    }
    tracing::warn!(port = port.name(), title_id = %game.title_id, "this port does not run without its toolchain here");
    Err(Text::new("ps.play.direct.port_unavailable").arg("name", port.name()))
}

/// The folder picked in Ajustes, else that of the build or version chosen for Jugar when it is in the
/// library. `None` when there is neither, which the window answers by downloading the newest release
/// and a headless run by not starting.
pub async fn chosen_dir(root: &Root, settings: &LauncherSettings) -> Option<PathBuf> {
    if let Some(dir) = settings.text(FOLDER_KEY) {
        return Some(PathBuf::from(dir));
    }
    let selected = settings.text(SELECTED_KEY);
    if let Some(commit) = selected.and_then(Commit::try_new)
        && winbuild::built_commit(root, &commit).await.is_some()
    {
        return Some(winbuild::dir(root, &commit));
    }
    if let Some(version) = selected.and_then(|tag| tag.parse::<Version>().ok())
        && package::installed_version(root, &version).await.is_some()
    {
        return Some(package::dir(root, &version));
    }
    None
}

/// `dir`'s `psport.exe` with the game's environment, `overrides` over it, and `run.sh`'s arguments,
/// ready to spawn in its session, and the facts for its log's header.
pub async fn session(
    root: &Root,
    game: &Game,
    inputs: &LaunchInputs,
    cache_dir: Option<&Path>,
    dir: PathBuf,
    overrides: &[(String, String)],
) -> Result<(Prepared, Vec<(&'static str, String)>), Text> {
    // Checked again at each launch: the folder may have been moved or emptied since it was picked.
    let folder = match tokio::task::spawn_blocking(move || Folder::try_new(dir)).await {
        Ok(Ok(folder)) => folder,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "the psport.exe folder is no longer usable");
            return Err(Text::new("ps.play.direct.bad_folder").text("error", crate::state::ps_folder_error(&err)));
        }
        Err(err) => return Err(Text::new("ps.errors.environment").arg("error", err)),
    };
    let mut launch = Launch::new(folder.executable(), session_key(&game.dir));
    launch.capture_output = true;
    let mut prepared = match decanter_runtime::prepare(launch).await {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::warn!(executable = %folder.executable().display(), error = %format_args!("{err:#}"), "could not prepare psport.exe");
            return Err(Text::new("ps.play.prepare_error")
                .arg("name", Port::Bbport.name())
                .arg("error", format_args!("{err:#}")));
        }
    };
    let built = Bbport::new(root.clone())
        .direct_command(
            prepared.command_mut(),
            &folder,
            &inputs.game_launch(game, cache_dir),
            overrides,
        )
        .await;
    if let Err(err) = built {
        tracing::warn!(title_id = %game.title_id, %err, "could not set up psport.exe's launch");
        return Err(Text::new("ps.errors.environment").arg("error", err));
    }
    let facts = vec![
        ("port", Port::Bbport.name().to_owned()),
        ("psport_folder", folder.path().display().to_string()),
    ];
    Ok((prepared, facts))
}

/// One session per dump folder, as one job per game.
pub fn session_key(dir: &Path) -> String {
    format!("ps:{}", dir.display())
}
