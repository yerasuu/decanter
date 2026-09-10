//! Running Windows games under Proton, through `umu-run`.
//!
//! umu is the launcher Valve's own Proton expects around it: it resolves
//! (and downloads) a Proton build, sets up the Steam Linux Runtime
//! container, and applies protonfixes for a known game id. decanter drives
//! it as a plain subprocess — there is no daemon, no agent inside the
//! prefix, and no state of umu's that decanter has to keep in sync.
//!
//! What that buys over talking to a prefix directly: one process to spawn
//! and one to wait on. Exit detection is the child's own exit plus a sweep
//! for anything it left behind (see [`Prefix::processes`]), rather than
//! polling an in-prefix service.

#![expect(
    clippy::implicit_return,
    reason = "directly contradicts needless_return from clippy::all, also enabled"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "most async fns here cost more than the 512-byte threshold by design"
)]

use core::str::from_utf8;
use core::time::Duration;
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Instant;

use anyhow::{Context as _, bail};
use futures_util::future::join_all;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::process::{Child, Command};
use tokio::time::sleep;

/// The launcher binary. Looked up on `PATH` rather than pinned: umu ships
/// as a distro package, and every distro puts it somewhere different.
const UMU_RUN: &str = "umu-run";

/// How often [`Prefix::wait_until_idle`] re-checks for leftover processes.
/// Only reached after the launcher already exited, so it costs one
/// directory scan per tick and nothing while a game is actually running.
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// A Proton prefix decanter owns.
///
/// umu treats `WINEPREFIX` as Proton's `STEAM_COMPAT_DATA_PATH` and then
/// symlinks `pfx` inside it back to the directory itself, so — unlike a
/// Steam `compatdata` layout — `drive_c` sits directly under this path.
#[derive(Debug, Clone)]
pub struct Prefix {
    /// Filesystem path to the prefix root.
    path: PathBuf,
}

impl Prefix {
    /// Where games get installed. Created on demand — umu creates it on
    /// first launch, but an install has to write files there before any
    /// launch has happened.
    #[inline]
    #[must_use]
    pub fn drive_c(&self) -> PathBuf {
        self.path.join("drive_c")
    }

    /// Creates the prefix if it isn't there yet, and returns once Wine has
    /// finished laying it out.
    ///
    /// Only needed before something other than a launch touches the
    /// prefix — an install writing into `drive_c`, most of all. A plain
    /// [`launch`] into a missing prefix creates it on the way.
    ///
    /// # Errors
    ///
    /// Returns an error if `wineboot` fails to spawn or exits with a
    /// non-zero status.
    #[inline]
    pub async fn initialize(&self, proton: Proton) -> anyhow::Result<()> {
        tracing::debug!(prefix = %self.path.display(), ?proton, "initializing prefix with wineboot");
        // `wineboot`, not a path to it: on a prefix that doesn't exist
        // there is nothing to point at yet, so Wine resolves it from the
        // Proton build instead.
        let mut launch = Launch::new("wineboot", "umu-decanter");
        launch.args.push("-i".to_owned());
        launch.proton = proton;
        launch.working_directory = Some(env::temp_dir());
        let session = match start(self, launch).await {
            Ok(session) => session,
            Err(err) => return Err(err),
        };
        session.wait().await.context("initializing the prefix with wineboot")
    }

    #[inline]
    pub fn new<P>(path: P) -> Self
    where
        P: Into<PathBuf>,
    {
        Self { path: path.into() }
    }

    #[inline]
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Pids of every live process running against this prefix, the
    /// launcher's own children included.
    ///
    /// Read off `/proc` rather than asked of the prefix: Wine propagates
    /// `WINEPREFIX` to everything it starts, pressure-vessel shares the
    /// host pid namespace, and this needs no Proton path, no in-prefix
    /// agent, and nothing running to answer.
    #[inline]
    #[must_use]
    pub fn processes(&self) -> Vec<u32> {
        let Ok(entries) = fs::read_dir("/proc") else {
            tracing::warn!("/proc unreadable, reporting no live processes for any prefix");
            return Vec::new();
        };
        let pids: Vec<u32> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let pid_str = match name.to_str() {
                    Some(pid_str) => pid_str,
                    None => return None,
                };
                let pid: u32 = match pid_str.parse().ok() {
                    Some(pid) => pid,
                    None => return None,
                };
                // Unreadable environ means someone else's process (or one
                // that exited mid-scan) — either way, not ours. A `let-else`
                // over a `match` with a bare `Err(_)` arm, which this
                // codebase's own humanization scanner rejects. Not logged:
                // this is the expected outcome for most of `/proc` on every
                // scan, so a per-pid line here would be noise, not signal.
                let Some(environ) = fs::read(entry.path().join("environ")).ok() else {
                    return None;
                };
                environ_has_prefix(&environ, &self.path).then_some(pid)
            })
            .collect();
        tracing::trace!(prefix = %self.path.display(), found = pids.len(), "scanned /proc for this prefix's processes");
        pids
    }

    /// Resolves a Windows path as stored on a program (`C:\Games\x.exe`)
    /// to the host path umu should be handed.
    ///
    /// Only `C:` resolves: that's the only drive decanter ever creates a
    /// program under, and mapping an arbitrary drive letter would mean
    /// reading `dosdevices`, which is the prefix's business, not ours.
    #[inline]
    #[must_use]
    pub fn resolve_windows_path(&self, windows_path: &str) -> Option<PathBuf> {
        let Some(rest) = windows_path
            .strip_prefix("C:\\")
            .or_else(|| windows_path.strip_prefix("c:\\"))
        else {
            tracing::warn!(windows_path, "rejecting windows path: not on the C: drive");
            return None;
        };
        let Some(relative) = sanitize_relative_path(&rest.replace('\\', "/")) else {
            tracing::warn!(windows_path, "rejecting windows path: escapes the prefix");
            return None;
        };
        Some(self.drive_c().join(relative))
    }

    /// Asks everything in the prefix to exit, then waits for it to.
    ///
    /// `SIGTERM`, not `SIGKILL`: Wine's own processes flush and shut the
    /// prefix down cleanly on it, and a game killed outright is a game
    /// that loses whatever it hadn't written yet.
    ///
    /// # Errors
    ///
    /// Never actually fails today — kept as a `Result` so a future
    /// signal-delivery failure can surface without an API break.
    #[inline]
    pub async fn stop(&self) -> anyhow::Result<()> {
        tracing::debug!(prefix = %self.path.display(), "stopping prefix");
        // Dropped from the live set first: a stop that is already under
        // way shouldn't be started a second time by a shutdown racing it.
        live_lock().remove(&self.path);
        // Re-signals every tick rather than sweeping once: a process that
        // appears after the first sweep (a game re-execing itself during
        // shutdown, which GOG's own launchers do) would otherwise never
        // receive SIGTERM, and the loop below would wait on it forever.
        loop {
            let pids = self.processes();
            if pids.is_empty() {
                break;
            }
            for pid in pids {
                // A pid outside `Pid`'s range can't belong to a real
                // process; skip rather than error, same as an already-gone one.
                let Ok(signed_pid) = i32::try_from(pid) else {
                    tracing::warn!(pid, "pid out of range for a signal, skipping");
                    continue;
                };
                // Racy by nature — the pid may already be gone, which is
                // the outcome being asked for anyway, so a failed send is
                // traced rather than warned about.
                let result = kill(Pid::from_raw(signed_pid), Signal::SIGTERM);
                tracing::trace!(pid, ?result, "sent SIGTERM");
            }
            sleep(IDLE_POLL_INTERVAL).await;
        }
        tracing::debug!(prefix = %self.path.display(), "prefix has no live processes left");
        Ok(())
    }

    /// Resolves once the prefix has no live processes left.
    ///
    /// A game that re-execs itself and detaches (GOG's own launchers do)
    /// outlives the `umu-run` that started it, so waiting on the launcher
    /// alone reports the session as over while the game is still up.
    #[inline]
    pub async fn wait_until_idle(&self) {
        let started = Instant::now();
        while !self.processes().is_empty() {
            sleep(IDLE_POLL_INTERVAL).await;
        }
        tracing::debug!(prefix = %self.path.display(), elapsed = ?started.elapsed(), "prefix went idle");
    }
}

/// Which Proton build to run against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Proton {
    /// Let umu pick and download the current build. What a prefix with no
    /// explicit choice gets.
    #[default]
    Latest,
    /// A build installed for Steam (see [`steam_runners`]), or one of
    /// umu's own version keywords (`GE-Proton`, `UMU-Proton`, ...).
    /// Passed through untouched: umu resolves a bare name against
    /// [`steam_compat_dir`] first, and treats what's left as a keyword to
    /// download.
    Named(String),
    /// A Proton directory on disk, used as-is.
    Path(PathBuf),
}

impl Proton {
    /// Value for umu's `PROTONPATH` env var, or `None` to leave it unset.
    fn as_env_value(&self) -> Option<&OsStr> {
        match self {
            // Unset, not empty: umu treats an empty PROTONPATH as "choose
            // for me", and an absent one the same way.
            Self::Latest => None,
            Self::Named(name) => Some(OsStr::new(name)),
            Self::Path(path) => Some(path.as_os_str()),
        }
    }
}

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
    /// Host path to the `.exe`. Use [`Prefix::resolve_windows_path`] to
    /// get one from a program's stored `C:\...` path.
    pub executable: PathBuf,
    /// umu's game identity, which is what protonfixes keys its per-game
    /// workarounds off. Convention is `umu-<storefront id>`.
    pub game_id: String,
    pub proton: Proton,
    /// umu's `STORE`, used alongside `game_id` to look up protonfixes.
    pub store: Option<String>,
    /// Defaults to the executable's own directory. Games that load data
    /// by relative path — most of them — need this, and Proton does not
    /// infer it.
    pub working_directory: Option<PathBuf>,
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
        P: Into<PathBuf>,
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

/// The prefixes decanter has started something in and not seen finish.
///
/// Kept apart from whatever the UI is showing, because this is what
/// shutdown reads: a game outlives the launcher that started it — umu
/// execs into a container and Proton detaches — so nothing stops on its
/// own when decanter goes away. Closing the app has to go and stop them.
fn live() -> &'static Mutex<HashSet<PathBuf>> {
    static LIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    LIVE.get_or_init(Default::default)
}

/// Locks [`live`], recovering a poisoned lock rather than discarding its
/// contents: a panic while holding it must not make `stop_all()` believe no
/// games are running and leave them alive.
fn live_lock() -> MutexGuard<'static, HashSet<PathBuf>> {
    live().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every prefix with a session in flight.
#[inline]
#[must_use]
pub fn live_prefixes() -> Vec<Prefix> {
    live_lock().iter().cloned().map(Prefix::new).collect()
}

/// Stops everything decanter started, at once.
///
/// For shutdown. Concurrent rather than one after another: each stop waits
/// out its own prefix, and closing the app shouldn't take as long as the
/// sum of them.
#[inline]
pub async fn stop_all() {
    let prefixes = live_prefixes();
    if prefixes.is_empty() {
        return;
    }
    tracing::info!("stopping {} running game(s)", prefixes.len());
    let stopping = prefixes.iter().map(Prefix::stop);
    join_all(stopping).await;
}

/// Where Steam (and umu) keep Proton builds.
///
/// umu resolves a bare `PROTONPATH` against this directory, so a build
/// that lives here can be named rather than pointed at by full path —
/// which is what [`Proton::Named`] carries.
#[inline]
#[must_use]
pub fn steam_compat_dir() -> Option<PathBuf> {
    let Some(data) = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
    else {
        tracing::warn!("neither XDG_DATA_HOME nor HOME is set, cannot resolve the Steam compat dir");
        return None;
    };
    Some(data.join("Steam").join("compatibilitytools.d"))
}

/// Every Proton build installed for Steam, newest-looking last — sorted by
/// name, which is as close to a version order as these get.
///
/// This is the list a prefix picks its runner from: Proton-GE, Proton-EM,
/// `CachyOS`'s builds, whatever the user installed. umu can also download
/// its own (see [`Proton::Latest`]), but a build that is already on disk
/// costs nothing and is the one the user chose.
#[inline]
#[must_use]
pub fn steam_runners() -> Vec<String> {
    let Some(directory) = steam_compat_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&directory) else {
        tracing::debug!(directory = %directory.display(), "no steam compat dir to scan, no runners found");
        return Vec::new();
    };
    let mut runners: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            // `proton` is the launcher script every Proton build ships;
            // without it this is a leftover directory, not a runner.
            let path = entry.path();
            let is_runner = path.join("proton").is_file();
            tracing::trace!(candidate = %path.display(), is_runner, "checking proton build candidate");
            is_runner
                .then(|| entry.file_name().to_str().map(str::to_owned))
                .flatten()
        })
        .collect();
    runners.sort();
    tracing::debug!(?runners, "resolved installed proton builds");
    runners
}

/// Whether `name` is a Proton build installed for Steam. Used to check a
/// stored choice still exists before launching against it.
#[inline]
#[must_use]
pub fn steam_runner_exists(name: &str) -> bool {
    let exists = steam_compat_dir().is_some_and(|directory| directory.join(name).join("proton").is_file());
    tracing::debug!(name, exists, "checked whether a named proton build is installed");
    exists
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
        None if is_bare_command => env::temp_dir(),
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

    let mut command = Command::new(UMU_RUN);
    command
        .arg(&launch.executable)
        .args(&launch.args)
        .current_dir(&working_directory)
        .env("WINEPREFIX", &prefix.path)
        .env("GAMEID", &launch.game_id)
        // Proton's default verb, set explicitly because it is what makes
        // umu-run outlive the game rather than returning immediately.
        .env("PROTON_VERB", "waitforexitandrun")
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
        .with_context(|| format!("spawning {UMU_RUN} (is umu-launcher installed?)"))
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

/// Whether a `/proc/<pid>/environ` blob belongs to a process running
/// against `prefix`.
///
/// The blob is NUL-separated `KEY=VALUE` entries. `WINEPREFIX` covers Wine
/// and the game; `STEAM_COMPAT_DATA_PATH` covers umu and Proton
/// themselves, which are already running before Wine sets anything.
fn environ_has_prefix(environ: &[u8], prefix: &Path) -> bool {
    let Some(prefix_str) = prefix.to_str() else {
        return false;
    };
    environ.split(|byte| *byte == 0).any(|bytes| {
        let Ok(text) = from_utf8(bytes) else {
            return false;
        };
        let Some((key, value)) = text.split_once('=') else {
            return false;
        };
        matches!(key, "WINEPREFIX" | "STEAM_COMPAT_DATA_PATH") && value == prefix_str
    })
}

/// Reduces a caller-supplied relative path to something that can only ever
/// land inside the directory it is joined to: no absolute paths, no `..`,
/// no drive prefixes.
///
/// Every path decanter joins onto a prefix or a data directory comes from
/// a storefront's manifest or a product id, i.e. from the network.
#[inline]
#[must_use]
pub fn sanitize_relative_path(path: &str) -> Option<PathBuf> {
    let source = Path::new(path);
    let mut sanitized = PathBuf::new();
    for component in source.components() {
        match component {
            Component::Normal(part) => sanitized.push(part),
            // `.` is meaningless rather than dangerous; the rest are how a
            // relative path escapes upward.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                tracing::warn!(path, "rejecting path: climbs out of or escapes its base directory");
                return None;
            }
        }
    }
    if sanitized.as_os_str().is_empty() {
        tracing::warn!(path, "rejecting path: sanitizes to nothing");
        return None;
    }
    Some(sanitized)
}

/// A one-shot process listing per prefix, for callers watching several at
/// once — `/proc` gets scanned once instead of once per prefix.
#[inline]
#[must_use]
pub fn processes_by_prefix(prefixes: &[Prefix]) -> HashMap<PathBuf, Vec<u32>> {
    let mut found: HashMap<PathBuf, Vec<u32>> = prefixes
        .iter()
        .map(|prefix| (prefix.path.clone(), Vec::new()))
        .collect();
    let Ok(entries) = fs::read_dir("/proc") else {
        tracing::warn!("/proc unreadable, reporting no live processes for any prefix");
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        // Same as `Prefix::processes`: unreadable for most of `/proc` on
        // every scan, so not logged per-pid.
        let Ok(environ) = fs::read(entry.path().join("environ")) else {
            continue;
        };
        for prefix in prefixes {
            if environ_has_prefix(&environ, &prefix.path) {
                found.entry(prefix.path.clone()).or_default().push(pid);
            }
        }
    }
    tracing::trace!(
        prefixes = prefixes.len(),
        matched = found.values().map(Vec::len).sum::<usize>(),
        "scanned /proc for several prefixes at once"
    );
    found
}
