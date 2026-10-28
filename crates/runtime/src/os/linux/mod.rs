//! Linux: Windows games run under Proton through `umu-run`, each in a prefix
//! of its own, and Linux programs run as themselves, with the prefix's path
//! only naming their session.

mod ge_proton;
mod prefix;
mod proton;
mod umu;

use std::path::Path;
use std::process::{ExitStatus, Stdio};

use anyhow::{Context as _, bail};
use decanter_core::runner::Runner;
use tokio::process::{Child, Command};

use crate::runner;
use crate::session::live_lock;
use crate::{Launch, Prefix, Prepared, Session};

pub(crate) use ge_proton::{ge_proton_prepare, ge_proton_remove, ge_proton_status, ge_proton_update};
use prefix::SESSION_ENV;
pub(crate) use prefix::{live_prefixes, prefix_processes, processes_by_prefix, stop_prefix};
pub(crate) use proton::{proton_status, steam_compat_dir};
pub(crate) use umu::{umu_on_path, umu_prepare, umu_remove, umu_status, umu_update};

/// What Proton sets `WINEDEBUG` to under `PROTON_LOG=1` (its `proton`
/// script, `Session.init_session`).
const PROTON_LOG_WINEDEBUG: &str = "+timestamp,+pid,+tid,+seh,+unwind,+threadname,+debugstr,+loaddll,+mscoree";

/// What the session's process is called in its exit's log lines: what
/// decanter starts here, for a Linux program run natively too.
pub(crate) const LAUNCHER: &str = "umu-run";

/// What a live session is found and stopped by: its prefix.
pub(crate) type Tracked = Prefix;

/// `umu-run` against the launch's prefix, or a Linux program as itself.
pub(crate) async fn prepare(mut launch: Launch) -> anyhow::Result<Prepared> {
    if !crate::available() {
        bail!(
            "Proton runs only on Linux, cannot start {}",
            launch.executable.display()
        );
    }
    let Some(prefix) = launch.prefix.clone() else {
        tracing::warn!(executable = %launch.executable.display(), game_id = %launch.game_id, "rejecting launch: no prefix");
        bail!("no prefix to run {} in", launch.executable.display());
    };
    // A bare name (`wineboot`) is left for Wine to resolve inside the
    // prefix; anything with a path in it is a host file that has to exist,
    // and catching that here beats a launch that silently does nothing.
    let is_bare_command = launch.executable.parent() == Some(Path::new(""));
    if !is_bare_command && !launch.executable.is_file() {
        tracing::warn!(
            executable = %launch.executable.display(),
            game_id = %launch.game_id,
            "rejecting launch: no such executable on disk"
        );
        bail!("no executable at {}", launch.executable.display());
    }

    let working_directory = match launch.working_directory.take() {
        Some(directory) => directory,
        None if is_bare_command => std::env::temp_dir(),
        None => match launch
            .executable
            .parent()
            .map(Path::to_path_buf)
            .context("executable has no parent directory")
        {
            Ok(parent) => parent,
            Err(err) => return Err(err),
        },
    };
    tracing::debug!(working_directory = %working_directory.display(), "resolved working directory for launch");

    if launch.native {
        return Ok(prepare_native(&prefix, launch, &working_directory));
    }

    let umu_run_path = runner::update(Runner::Umu, &mut |_| {})
        .await
        .context("ensuring umu-run is available")?
        .path;
    let mut command = wrapped(&umu_run_path, &mut launch);
    // umu/Proton build whatever environment the container needs
    // themselves; decanter's own (library path, allocator and EGL
    // settings) has only ever caused breakage in there.
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
        .arg(&launch.executable)
        .args(&launch.args)
        .current_dir(&working_directory)
        .env("WINEPREFIX", &prefix.path)
        .env("GAMEID", &launch.game_id)
        // Proton's default verb, set explicitly because it is what makes
        // umu-run outlive the game rather than returning immediately.
        .env("PROTON_VERB", "waitforexitandrun")
        // Xalia (Proton's UI-automation accessibility bridge) crashes with
        // "No displays available" on at least one real setup this was run
        // on, and Proton treats that crash as fatal to the whole launch --
        // confirmed against a real prefix: `umu-run` exited 3 with nothing
        // wrong in the game itself, only Xalia's own unhandled exception
        // just before it. decanter exposes no accessibility bridge of its
        // own for Xalia to serve, so there is nothing lost by turning it
        // off by default; `launch.env` below can still turn it back on.
        .env("PROTON_USE_XALIA", "0")
        .stdin(Stdio::null());

    // Proton stays silent about a crash inside Wine itself (a missing DLL,
    // a bad module load) unless asked: without `PROTON_LOG` it runs Wine
    // with `WINEDEBUG=-all`. A caller taking the output wants that detail
    // on stderr, where it arrives in order with umu's own lines, and
    // Proton keeps a `WINEDEBUG` it is handed. Otherwise `PROTON_LOG`
    // sends it to Proton's own log file, whose *filename* keys off
    // `SteamGameId`, so both have to be set for one to land anywhere
    // findable, even though a non-Steam game only has a `GAMEID` here.
    // Written beside the prefix rather than `$HOME` (Proton's default) so
    // it doesn't scatter one file per game across the account that runs
    // decanter.
    if launch.capture_output {
        command
            .env("WINEDEBUG", PROTON_LOG_WINEDEBUG)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    } else {
        command
            .env("PROTON_LOG", "1")
            .env("PROTON_LOG_DIR", &prefix.path)
            .env("SteamGameId", &launch.game_id);
    }

    if let Some(proton) = launch.proton.as_env_value() {
        command.env("PROTONPATH", proton);
    }
    if let Some(store) = launch.store.as_ref() {
        command.env("STORE", store);
    }
    if let Some(overrides) = format_dll_overrides(&launch.dll_overrides) {
        command.env("WINEDLLOVERRIDES", overrides);
    }
    command.envs(launch.env.iter().map(|entry| (entry.0.as_str(), entry.1.as_str())));

    // Not logged: `launch.env` is caller-supplied and, unlike the fields
    // above, has no fixed shape — nothing here rules out a future caller
    // routing a secret through it.
    tracing::debug!(
        executable = %launch.executable.display(),
        args = ?launch.args,
        game_id = %launch.game_id,
        store = ?launch.store,
        gamemode = launch.gamemoded.is_some(),
        prefix = %prefix.path.display(),
        "umu-run command built"
    );

    Ok(Prepared {
        command,
        launch,
        program: umu_run_path,
    })
}

pub(crate) fn spawn_failed(launch: &Launch) {
    tracing::error!(
        executable = %launch.executable.display(),
        game_id = %launch.game_id,
        native = launch.native,
        "failed to spawn the game's command"
    );
}

/// The launch's prefix, which every process of the session runs against.
pub(crate) fn spawned(child: &Child, launch: &Launch) -> Option<Tracked> {
    tracing::info!(
        pid = child.id(),
        game_id = %launch.game_id,
        proton = ?launch.proton,
        "launched game process"
    );
    launch.prefix.clone()
}

/// `umu-run`'s exit, then anything it left running in the prefix.
pub(crate) async fn wait_status(mut session: Session) -> anyhow::Result<ExitStatus> {
    let pid = session.child.id();
    let status = match session.child.wait().await.context("waiting for umu-run") {
        Ok(status) => status,
        Err(err) => {
            tracing::error!(pid, prefix = ?session.prefix.as_ref().map(Prefix::path), "failed to wait for umu-run");
            return Err(err);
        }
    };
    let elapsed = session.started_at.elapsed();
    if status.success() {
        tracing::info!(pid, exit_code = status.code(), elapsed = ?elapsed, "umu-run exited");
    } else {
        tracing::error!(pid, exit_code = status.code(), elapsed = ?elapsed, "umu-run exited with a failure status");
    }
    if let Some(prefix) = session.prefix.as_ref() {
        prefix.wait_until_idle().await;
    }
    live_lock().remove(&session.key);
    Ok(status)
}

/// [`Prefix::stop`] on the session's prefix.
pub(crate) async fn stop(key: &str) {
    let prefix = live_lock().get(key).cloned();
    let Some(prefix) = prefix else {
        tracing::debug!(key, "session has exited");
        return;
    };
    if let Err(err) = prefix.stop().await {
        tracing::error!(key, error = %format_args!("{err:#}"), "stop failed");
    }
}

/// The same as [`stop`]: a prefix's processes only ever get `SIGTERM`, which
/// Wine shuts a prefix down cleanly on, and nothing here kills one outright.
pub(crate) async fn end(key: &str) {
    stop(key).await;
}

/// `program` started in the game's own scope and under GameMode when the
/// launch asks for them.
fn wrapped(program: &Path, launch: &mut Launch) -> Command {
    let (key, description) = launch.identity.take().unwrap_or_else(|| {
        let name = launch.executable.file_name().unwrap_or_default();
        (launch.game_id.clone(), name.to_string_lossy().into_owned())
    });
    let gamemode = launch.gamemoded.as_deref().map(decanter_core::gamemode_prefix);
    match (decanter_core::own_scope(&key, &description), gamemode) {
        (Some(mut scoped), gamemode) => {
            scoped.args(gamemode.into_iter().flatten()).arg(program);
            Command::from(scoped)
        }
        (None, Some([shell, rest @ ..])) => {
            let mut command = Command::new(shell);
            command.args(rest).arg(program);
            command
        }
        (None, None) => Command::new(program),
    }
}

/// A Linux program started as itself, tagged with [`SESSION_ENV`] so the
/// session's processes are found the way a prefix's are.
fn prepare_native(prefix: &Prefix, mut launch: Launch, working_directory: &Path) -> Prepared {
    let executable = launch.executable.clone();
    let mut command = wrapped(&executable, &mut launch);
    // As under umu: decanter's own library path and allocator settings are
    // not the game's.
    decanter_core::strip_launcher_env(command.as_std_mut());
    command
        .args(&launch.args)
        .current_dir(working_directory)
        .env(SESSION_ENV, &prefix.path)
        .stdin(Stdio::null());
    if launch.capture_output {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    command.envs(launch.env.iter().map(|entry| (entry.0.as_str(), entry.1.as_str())));
    tracing::debug!(
        executable = %executable.display(),
        args = ?launch.args,
        game_id = %launch.game_id,
        gamemode = launch.gamemoded.is_some(),
        session = %prefix.path.display(),
        "native command built"
    );
    Prepared {
        command,
        launch,
        program: executable,
    }
}

/// Builds the `WINEDLLOVERRIDES` value, or `None` when there is nothing to
/// override — an empty value is not the same as an unset one to Wine.
fn format_dll_overrides(overrides: &[(String, String)]) -> Option<String> {
    if overrides.is_empty() {
        return None;
    }
    // Wine's own grouping (`a,b=n;c=b`) is only shorter, never different,
    // so each override gets its own entry.
    Some(
        overrides
            .iter()
            .map(|entry| format!("{}={}", entry.0, entry.1))
            .collect::<Vec<_>>()
            .join(";"),
    )
}
