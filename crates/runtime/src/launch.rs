//! One game launch: the parameters ([`Launch`]), spawning it ([`start`]),
//! and the running result ([`Session`]).

use std::path::Path;
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context as _, bail};
use tokio::process::{Child, Command};

use crate::prefix::{Prefix, live_lock};
use crate::proton::Proton;
use crate::umu::ensure_umu_run;

/// One game launch.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Launch {
    pub args: Vec<String>,
    /// `(dll name, mode)` pairs, e.g. `("Galaxy64", "n,b")`.
    ///
    /// Only needed to override a DLL Wine itself provides: a proxy sitting
    /// in the game's own directory already wins, because that is where
    /// Wine looks first for a non-system DLL.
    pub dll_overrides: Vec<(String, String)>,
    /// Extra environment, applied last — so it wins over everything above.
    pub env: Vec<(String, String)>,
    /// Host path to the `.exe`. Use [`crate::Prefix::resolve_windows_path`]
    /// to get one from a program's stored `C:\...` path.
    pub executable: std::path::PathBuf,
    /// umu's game identity, which is what protonfixes keys its per-game
    /// workarounds off. Convention is `umu-<storefront id>`.
    pub game_id: String,
    pub proton: Proton,
    /// umu's `STORE`, used alongside `game_id` to look up protonfixes.
    pub store: Option<String>,
    /// Defaults to the executable's own directory. Games that load data
    /// by relative path — most of them — need this, and Proton does not
    /// infer it.
    pub working_directory: Option<std::path::PathBuf>,
}

impl Launch {
    #[inline]
    #[must_use]
    pub fn arg<S>(mut self, arg: S) -> Self
    where
        S: Into<String>,
    {
        self.args.push(arg.into());
        self
    }

    #[inline]
    #[must_use]
    pub fn dll_override<D, M>(mut self, dll: D, mode: M) -> Self
    where
        D: Into<String>,
        M: Into<String>,
    {
        self.dll_overrides.push((dll.into(), mode.into()));
        self
    }

    #[inline]
    #[must_use]
    pub fn env<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.env.push((key.into(), value.into()));
        self
    }

    #[inline]
    pub fn new<P, G>(executable: P, game_id: G) -> Self
    where
        P: Into<std::path::PathBuf>,
        G: Into<String>,
    {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            working_directory: None,
            game_id: game_id.into(),
            store: None,
            proton: Proton::default(),
            dll_overrides: Vec::new(),
            env: Vec::new(),
        }
    }

    #[inline]
    #[must_use]
    pub fn proton(mut self, proton: Proton) -> Self {
        self.proton = proton;
        self
    }

    #[inline]
    #[must_use]
    pub fn store<S>(mut self, store: S) -> Self
    where
        S: Into<String>,
    {
        self.store = Some(store.into());
        self
    }
}

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
    pub async fn wait(mut self) -> anyhow::Result<()> {
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
        if !status.success() {
            bail!("umu-run exited with {status}");
        }
        Ok(())
    }
}

/// Starts `launch` in `prefix`, returning as soon as umu is running.
///
/// The prefix is created by umu on first use, so a path that doesn't exist
/// yet is fine — that is how a new prefix gets made.
///
/// # Errors
///
/// Returns an error if `launch.executable` isn't a bare command and
/// doesn't exist as a host file, if its working directory can't be
/// resolved, or if `umu-run` fails to spawn.
#[inline]
pub async fn start(prefix: &Prefix, mut launch: Launch) -> anyhow::Result<Session> {
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

    let umu_run_path = ensure_umu_run().await.context("ensuring umu-run is available")?;
    let mut command = Command::new(&umu_run_path);
    command
        .arg(&launch.executable)
        .args(&launch.args)
        .current_dir(&working_directory)
        // A game never needs decanter's own library path (set for its
        // graphics dependencies, e.g. under Nix); umu/Proton build
        // whatever environment the container needs internally, and
        // leaking this one in has caused real, unrelated breakage before
        // (see `decanter_core::steam::process::sanitize_env` for the same
        // fix on the Steam-launch side).
        .env_remove("LD_LIBRARY_PATH")
        .env_remove("LD_PRELOAD")
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
        // Proton stays silent about a crash inside Wine itself (a missing
        // DLL, a bad module load) unless asked: without `PROTON_LOG` its
        // own wrapper never opens a log file at all, and `SteamGameId` is
        // what that log's *filename* keys off, so both have to be set for
        // one to land anywhere findable, even though a non-Steam game only
        // has a `GAMEID` here otherwise. Written beside the prefix rather
        // than `$HOME` (Proton's own default) so it doesn't scatter one
        // file per game across the account that runs decanter.
        .env("PROTON_LOG", "1")
        .env("PROTON_LOG_DIR", &prefix.path)
        .env("SteamGameId", &launch.game_id)
        .stdin(Stdio::null());

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
        prefix = %prefix.path.display(),
        "spawning umu-run"
    );

    let child = match command
        .spawn()
        .with_context(|| format!("spawning {}", umu_run_path.display()))
    {
        Ok(child) => child,
        Err(err) => {
            tracing::error!(
                executable = %launch.executable.display(),
                game_id = %launch.game_id,
                "failed to spawn umu-run"
            );
            return Err(err);
        }
    };
    let started_at = Instant::now();

    tracing::info!(
        pid = child.id(),
        game_id = %launch.game_id,
        proton = ?launch.proton,
        "launched game process"
    );

    live_lock().insert(prefix.path.clone());

    Ok(Session {
        child,
        prefix: prefix.clone(),
        started_at,
    })
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
