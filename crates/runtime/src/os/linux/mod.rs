//! Linux: Windows games run under Proton through `umu-run`, each in a prefix
//! of its own, and Linux programs run as themselves, with the prefix's path
//! only naming their session.

mod prefix;
mod proton;
mod umu;

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Instant;

use anyhow::{Context as _, bail};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

use crate::{Launch, Prefix};

use prefix::{SESSION_ENV, live_lock};
pub use prefix::{live_prefixes, processes_by_prefix, stop_all};
pub use proton::{UMU_PROTON_REPO, steam_compat_dir, steam_runner_exists, steam_runners};
use umu::ensure_umu_run;
pub use umu::{UMU_LAUNCHER_REPO, UMU_LAUNCHER_VERSION, umu_run_path};

/// What Proton sets `WINEDEBUG` to under `PROTON_LOG=1` (its `proton`
/// script, `Session.init_session`).
const PROTON_LOG_WINEDEBUG: &str = "+timestamp,+pid,+tid,+seh,+unwind,+threadname,+debugstr,+loaddll,+mscoree";

/// A launched game.
///
/// Dropping this detaches rather than kills — the game keeps running, and
/// the prefix is still reachable through [`Prefix::processes`]. Use
/// [`Prefix::stop`] to actually end a session.
pub struct Session {
    /// The `umu-run` child process.
    child: Child,
    /// The prefix this session is running against.
    prefix: Prefix,
    /// When the child was spawned, so [`Session::wait`] can log how long
    /// the session ran without asking the OS for the child's own start time.
    started_at: Instant,
}

impl Session {
    /// The `umu-run` process, not the game: umu execs into the runtime
    /// container and Proton starts the game below that. Useful for logging
    /// and for [`Session::wait`], not as a handle on the game itself.
    #[inline]
    #[must_use]
    pub fn launcher_pid(&self) -> Option<u32> {
        self.child.id()
    }

    #[inline]
    #[must_use]
    pub const fn prefix(&self) -> &Prefix {
        &self.prefix
    }

    /// The launcher's pipes, once each, when the launch asked for
    /// [`Launch::capture_output`]; `None` otherwise. The game inherits
    /// them, so they stay open until the last process in the prefix is
    /// gone — read them to the end, or the game blocks on a full pipe.
    #[inline]
    pub const fn take_output(&mut self) -> (Option<ChildStdout>, Option<ChildStderr>) {
        (self.child.stdout.take(), self.child.stderr.take())
    }

    /// Waits for the whole session: the launcher first, then anything it
    /// left running in the prefix.
    ///
    /// A non-zero launcher exit is reported, but only after the prefix has
    /// gone idle — the games that fail this way have usually started
    /// something that is still up.
    ///
    /// # Errors
    ///
    /// Returns an error if the launcher can't be waited on, or if it
    /// exits with a non-zero status.
    #[inline]
    pub async fn wait(self) -> anyhow::Result<()> {
        let status = match self.wait_status().await {
            Ok(status) => status,
            Err(err) => return Err(err),
        };
        if !status.success() {
            bail!("umu-run exited with {status}");
        }
        Ok(())
    }

    /// [`Session::wait`] for a caller that records how the launcher ended
    /// rather than treating a non-zero exit as its own failure.
    ///
    /// # Errors
    ///
    /// Returns an error if the launcher can't be waited on.
    #[inline]
    pub async fn wait_status(mut self) -> anyhow::Result<ExitStatus> {
        let pid = self.child.id();
        let status = match self.child.wait().await.context("waiting for umu-run") {
            Ok(status) => status,
            Err(err) => {
                tracing::error!(pid, prefix = %self.prefix.path.display(), "failed to wait for umu-run");
                return Err(err);
            }
        };
        let elapsed = self.started_at.elapsed();
        if status.success() {
            tracing::info!(pid, exit_code = status.code(), elapsed = ?elapsed, "umu-run exited");
        } else {
            tracing::error!(pid, exit_code = status.code(), elapsed = ?elapsed, "umu-run exited with a failure status");
        }
        self.prefix.wait_until_idle().await;
        live_lock().remove(&self.prefix.path);
        Ok(status)
    }
}

/// A launch with its `umu-run` command built but not started, so a caller
/// can record the command before it runs. [`Prepared::spawn`] starts it.
pub struct Prepared {
    /// `umu-run` with everything [`prepare`] put on it.
    command: Command,
    /// What it was prepared from, for the spawn's own log lines.
    launch: Launch,
    /// The prefix it runs against, handed to the [`Session`].
    prefix: Prefix,
    /// What the command starts (`umu-run`, or a native program itself),
    /// named when the spawn fails.
    program: PathBuf,
}

impl Prepared {
    /// The command as it will be spawned: program, arguments, working
    /// directory and the environment decanter sets on top of its own.
    #[inline]
    pub const fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Starts the command, returning as soon as umu is running.
    ///
    /// # Errors
    ///
    /// Returns an error if `umu-run` fails to spawn.
    #[inline]
    pub fn spawn(mut self) -> anyhow::Result<Session> {
        let child = match self
            .command
            .spawn()
            .with_context(|| format!("spawning {}", self.program.display()))
        {
            Ok(child) => child,
            Err(err) => {
                tracing::error!(
                    executable = %self.launch.executable.display(),
                    game_id = %self.launch.game_id,
                    native = self.launch.native,
                    "failed to spawn the game's command"
                );
                return Err(err);
            }
        };
        let started_at = Instant::now();

        tracing::info!(
            pid = child.id(),
            game_id = %self.launch.game_id,
            proton = ?self.launch.proton,
            "launched game process"
        );

        live_lock().insert(self.prefix.path.clone());

        Ok(Session {
            child,
            prefix: self.prefix,
            started_at,
        })
    }
}

/// Starts `launch` in its prefix, returning as soon as umu is running.
///
/// # Errors
///
/// Returns an error off Linux, without a prefix, if `launch.executable`
/// isn't a bare command and doesn't exist as a host file, if its working
/// directory can't be resolved, or if `umu-run` fails to spawn.
#[inline]
pub async fn start(launch: Launch) -> anyhow::Result<Session> {
    match prepare(launch).await {
        Ok(prepared) => prepared.spawn(),
        Err(err) => Err(err),
    }
}

/// [`start`] up to the spawn: everything that can fail before the process
/// exists, with the command in hand for a caller to look at first.
///
/// # Errors
///
/// Returns an error off Linux, without a prefix, if `launch.executable`
/// isn't a bare command and doesn't exist as a host file, or if its working
/// directory can't be resolved.
#[inline]
pub async fn prepare(mut launch: Launch) -> anyhow::Result<Prepared> {
    if !crate::available() {
        bail!(
            "Proton runs only on Linux, cannot start {}",
            launch.executable.display()
        );
    }
    let Some(prefix) = launch.prefix.take() else {
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
        return Ok(prepare_native(prefix, launch, &working_directory));
    }

    let umu_run_path = ensure_umu_run().await.context("ensuring umu-run is available")?;
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
        prefix,
        program: umu_run_path,
    })
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
fn prepare_native(prefix: Prefix, mut launch: Launch, working_directory: &Path) -> Prepared {
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
        prefix,
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
