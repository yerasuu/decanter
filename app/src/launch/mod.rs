//! Starting and stopping a game, shared by the library's PLAY button, the
//! right-click menu, and the compatibility screen.
//!
//! The running-state bookkeeping lives here once instead of at each of
//! those call sites. Every caller runs [`start`] as a detached app-level
//! task, not a view's: it writes to `running`, which swaps the button that
//! started it out of existence.
//!
//! How a game is started is its [`Kind`]'s, one module each with the same
//! functions: Proton in a prefix of its own (`umu`), a Linux program run as
//! itself (`linux_native`), the system's own program on Windows and macOS
//! (`native`), or a program of the Raven section run by Raven (`raven`).
//! Every kind is built on every system; which one a game takes is decided
//! when it launches, from the system, where its manifest is filed and its file.

mod linux_native;
mod native;
mod pe;
mod raven;
mod umu;

use std::path::{Path, PathBuf};

pub use umu::reset_prefix;

use decanter_core::capabilities::Feature;
use decanter_core::{InstalledGame, platform};
use decanter_ps::process::{self, Event, Job, JobKind};
use decanter_ps::run_log::{RunLog, describe_exit};
use decanter_runtime::{Launch, Prepared};
use gpui::{AppContext as _, AsyncApp, Entity};
use rust_i18n::t;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::state::Background;

/// How one game is started.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A Windows program under Proton through `umu-run`, in its own prefix.
    Umu,
    /// A Linux program run as itself, its prefix only naming its session.
    LinuxNative,
    /// A program of the system's own (Windows, macOS), from its folder.
    Native,
    /// A Windows program of the Raven section, run by Raven in its own prefix.
    Raven,
}

impl Kind {
    /// The kind a Windows game takes here: under Proton where the system
    /// offers it (Linux), else as the program it is.
    fn host() -> Self {
        if platform::support(Feature::Proton).is_offered() {
            Self::Umu
        } else {
            Self::Native
        }
    }

    /// Read from the file rather than kept in the manifest: what decides is
    /// what the executable is now, and only an external game can be a Linux
    /// program.
    fn of(game: &InstalledGame, executable: &Path) -> Self {
        if crate::raven::is_program(&game.account_id, &game.game_id) {
            return Self::Raven;
        }
        let host = Self::host();
        if host == Self::Umu
            && game.storefront == decanter_core::Storefront::Unspecified
            && crate::external::probe::is_native_linux(executable)
        {
            Self::LinuxNative
        } else {
            host
        }
    }
}

/// Whether games here run in a prefix of their own, which everything about
/// prefixes and Proton (their settings, tabs and imports) exists for.
pub fn in_prefixes() -> bool {
    Kind::host() == Kind::Umu
}

/// A launch resolved before anything is synced or spawned, so a game with
/// nowhere to run fails before its cloud saves are touched.
struct Planned {
    launch: Launch,
    /// What a run that never started couldn't start, for its log.
    launcher: &'static str,
    /// Where decanter-hook writes this session's unlocks; GOG games only.
    achievement_log: Option<PathBuf>,
    /// Header lines for the run's log.
    facts: Vec<(&'static str, String)>,
    /// The game's key, its library's root and whether MangoHud logs from
    /// the start, when it runs with MangoHud.
    mangohud: Option<(String, PathBuf, bool)>,
    /// The launch is of the Windows program, which [`Planned::prepare`] turns
    /// into Raven's own, since finding the toolchain that built Raven awaits.
    is_raven: bool,
}

impl Planned {
    fn plan(kind: Kind, account_id: &str, game: &InstalledGame, executable: PathBuf) -> Option<Self> {
        match kind {
            Kind::Umu => umu::plan(account_id, game, executable),
            Kind::LinuxNative => linux_native::plan(account_id, game, executable),
            Kind::Native => native::plan(account_id, game, executable),
            Kind::Raven => raven::plan(account_id, game, executable),
        }
    }

    async fn prepare(mut self) -> anyhow::Result<Prepared> {
        if let Some((key, library_root, autostart_log)) = self.mangohud.take() {
            self.launch.env.extend(
                crate::game_tools::mangohud_env(&key, &library_root, autostart_log)
                    .await
                    .map(|(name, value)| (name.to_owned(), value)),
            );
        }
        if self.is_raven {
            self.launch = decanter_runtime::runner::raven_launch(self.launch).await?;
        }
        decanter_runtime::prepare(self.launch).await
    }
}

/// Per account, the same as a game's manifest: two profiles owning one
/// game run it as two sessions.
fn session_key(account_id: &str, game_id: &str) -> String {
    format!("{account_id}/{game_id}")
}

/// What a game gets on Linux however it starts there: GameMode, MangoHud,
/// its scope's name, captured output and its shader caches. MangoHud's
/// variables wait for [`Planned::prepare`], since reading them awaits.
fn session_tools(account_id: &str, game: &InstalledGame, launch: &mut Launch) -> Option<(String, PathBuf, bool)> {
    let key = crate::taskbar::key(game.storefront, &game.game_id);
    let (mangohud, gamemoded) = crate::core::db()
        .map(|db| {
            (
                crate::game_tools::mangohud(&db, &key),
                crate::game_tools::gamemoded(&db, &key),
            )
        })
        .unwrap_or_default();
    launch.gamemoded = gamemoded;
    let mangohud = mangohud.and_then(|autostart_log| match crate::core::library_for_account(account_id) {
        Ok(library) => Some((key.clone(), library.root().to_path_buf(), autostart_log)),
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "no library to log MangoHud to, starting without it");
            None
        }
    });
    launch.identity = Some((key, game.name.clone()));
    launch.capture_output = true;
    launch.env.extend(crate::cache::launch_env(game));
    mangohud
}

/// Tells decanter-hook which product it records for and where to write,
/// which is what lets decanter find what it wrote afterwards.
fn hook_env(launch: &mut Launch, game_id: &str, achievement_log: &Path) {
    launch.env.push(("DECANTER_PRODUCT_ID".to_owned(), game_id.to_owned()));
    launch.env.push((
        "DECANTER_ACHIEVEMENT_LOG".to_owned(),
        achievement_log.display().to_string(),
    ));
}

fn set_running(background: &Entity<Background>, game_id: &str, running: bool, cx: &mut AsyncApp) {
    background.update(cx, |background, cx| {
        if running {
            background.running.insert(game_id.to_owned());
        } else {
            background.running.remove(game_id);
        }
        cx.notify();
    });
}

/// Runs `game` to completion, holding it in `running` for as long as its
/// session lasts.
///
/// Returns how long the session lasted, or `None` if it never started —
/// the caller decides whether that's worth recording as playtime.
pub async fn start(
    account_id: String,
    game: InstalledGame,
    background: Entity<Background>,
    cx: &mut AsyncApp,
) -> Option<std::time::Duration> {
    let Some(executable) = crate::core::executable_for(&game) else {
        tracing::error!(game_id = %game.game_id, "no launch executable");
        return None;
    };
    // A reconcile that fails stops the launch, since starting the game without the mods its
    // player enabled would look like they work.
    let play = match crate::mods::prepare(&account_id, &game).await {
        Ok(play) => play,
        Err(reason) => {
            tracing::error!(game_id = %game.game_id, %reason, "launch failed");
            show_failure(&background, &game.game_id, &reason, cx);
            return None;
        }
    };
    let executable = match play.as_ref().map(|play| play.rebase(&executable)) {
        Some(Some(in_play)) => in_play,
        Some(None) => {
            tracing::error!(game_id = %game.game_id, path = %executable.display(), "launch executable is outside the install");
            return None;
        }
        None => executable,
    };
    if !executable.is_file() {
        tracing::error!(game_id = %game.game_id, path = %executable.display(), "launch executable is not on disk");
        return None;
    }
    let kind = Kind::of(&game, &executable);
    if kind == Kind::LinuxNative && !allowed_to_run(&game.game_id, &executable, &background, cx) {
        return None;
    }
    let hinted = executable.clone();
    let Some(mut planned) = Planned::plan(kind, &account_id, &game, executable) else {
        return None;
    };
    if let Some(play) = &play {
        planned.facts.extend(play.facts());
        if kind == Kind::Umu {
            planned.launch.env.extend(play.share_env());
        }
    }
    let saves_view = play.as_ref().and_then(|play| play.saves_view(&game));
    let saves_game = saves_view.as_ref().unwrap_or(&game);

    let cloud_save_checkpoint = sync_cloud_saves_before_launch(&account_id, saves_game).await;
    let achievement_log = planned.achievement_log.take();

    let Some((job, events, log)) = spawn(&account_id, &game, planned, &background, cx).await else {
        return None;
    };

    set_running(&background, &game.game_id, true, cx);
    let target = crate::closing::Target::Pc {
        account_id: account_id.clone(),
        game_id: game.game_id.clone(),
    };
    cx.update(|cx| crate::closing::started(target.clone(), game.name.clone(), cx));
    let started = std::time::Instant::now();
    let (exit, log) = crate::run_log::follow(&job, events, log, &game.game_id, cx).await;
    let log = match kind {
        Kind::Native => native::hint_after_failure(&hinted, exit, &job, log, &game.game_id, cx).await,
        Kind::Umu | Kind::LinuxNative | Kind::Raven => log,
    };
    crate::run_log::finish(log, exit, &game.game_id).await;
    cx.update(|cx| crate::closing::ended(&target, cx));
    set_running(&background, &game.game_id, false, cx);
    let elapsed = started.elapsed();
    tracing::info!(
        game_id = %game.game_id,
        exit = describe_exit(exit),
        duration_secs = elapsed.as_secs(),
        "game session ended"
    );

    sync_cloud_saves_after_exit(&account_id, saves_game, cloud_save_checkpoint).await;
    record_last_played(&account_id, &game, kind);
    apply_hook_log(&account_id, &game, achievement_log.as_deref()).await;
    Some(elapsed)
}

/// Starts `launch` with its output followed into the game's run, which the
/// library's Registro tab shows; `None` when it never started, with the
/// reason in that run.
async fn spawn(
    account_id: &str,
    game: &InstalledGame,
    mut planned: Planned,
    background: &Entity<Background>,
    cx: &mut AsyncApp,
) -> Option<(Entity<Job>, UnboundedReceiver<Event>, Option<RunLog>)> {
    let facts = core::mem::take(&mut planned.facts);
    let launcher = planned.launcher;
    let mut prepared = match planned.prepare().await {
        Ok(prepared) => prepared,
        Err(err) => {
            tracing::error!(game_id = %game.game_id, error = %format_args!("{err:#}"), "launch failed");
            show_failure(background, &game.game_id, &format!("{err:#}"), cx);
            return None;
        }
    };
    // A run goes on without its file rather than not at all; the log view says so.
    let (log, log_error) = match crate::run_log::start(account_id, &game.game_id, &facts, &mut prepared).await {
        Ok(log) => (Some(log), None),
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, %err, "could not create the run's log file");
            (None, Some(t!("ps.jobs.log_error", error = err).into_owned()))
        }
    };

    // Before the window can appear, so the task bar already knows it.
    crate::taskbar::sync_pc(game).await;
    let mut session = match prepared.spawn() {
        Ok(session) => session,
        Err(err) => {
            tracing::error!(game_id = %game.game_id, error = %format_args!("{err:#}"), "launch failed");
            let reason = format!("{err:#}");
            if let Some(log) = log {
                crate::run_log::abandon(log, &game.game_id, launcher, &reason).await;
            }
            show_failure(background, &game.game_id, &reason, cx);
            return None;
        }
    };
    tracing::info!(
        game_id = %game.game_id,
        name = %game.name,
        log = log.as_ref().map(|log| tracing::field::display(log.path().display())),
        "game launched"
    );
    let (stdout, stderr) = session.take_output();
    let waited = game.game_id.clone();
    let (mut job, events) = process::follow(JobKind::Game, session.launcher_pid(), stdout, stderr, async move {
        match session.wait_status().await {
            Ok(status) => Some(status),
            Err(err) => {
                tracing::warn!(game_id = %waited, error = %format_args!("{err:#}"), "game session ended with an error");
                None
            }
        }
    });
    job.log_file = log.as_ref().map(|log| log.path().to_owned());
    if let Some(error) = log_error {
        job.push(Event::Line(error));
    }
    let job = cx.new(|_| job);
    background.update(cx, |background, cx| {
        background.runs.insert(game.game_id.clone(), job.clone());
        cx.notify();
    });

    Some((job, events, log))
}

/// [`linux_native::allow_to_run`], with why it couldn't shown where the game's runs are.
#[cfg(unix)]
fn allowed_to_run(game_id: &str, executable: &Path, background: &Entity<Background>, cx: &mut AsyncApp) -> bool {
    let Err(err) = linux_native::allow_to_run(executable) else {
        return true;
    };
    tracing::error!(game_id, path = %executable.display(), %err, "could not give the program the permission to run");
    let reason = t!("external.errors.exec_bit", path = executable.display(), error = err);
    show_failure(background, game_id, &reason, cx);
    false
}

#[cfg(not(unix))]
const fn allowed_to_run(_: &str, _: &Path, _: &Entity<Background>, _: &mut AsyncApp) -> bool {
    true
}

/// Puts why `game_id` never started where its runs show, since no run of it began.
fn show_failure(background: &Entity<Background>, game_id: &str, reason: &str, cx: &mut AsyncApp) {
    let job = cx.new(|_| Job::failed(JobKind::Game, t!("ps.jobs.start_error", error = reason).into_owned()));
    background.update(cx, |background, cx| {
        background.runs.insert(game_id.to_owned(), job);
        cx.notify();
    });
}

/// Folds in whatever decanter-hook recorded this session: a locally
/// witnessed achievement unlock GOG's own servers never hear about without
/// a Galaxy client, and any failure to install its vtable patch in the
/// first place. The poll runs either way, which is how decanter hears of
/// what a running Galaxy client reported for the game.
/// Runs after [`record_last_played`], not before: neither ordering
/// matters to the other, so this just goes last as the newest addition.
///
/// Polls GOG first, same as decanter's own CLI launch path already does
/// after every session -- [`decanter_gog::hook::process_log`] can only
/// attach a local unlock to an achievement this game's cache already
/// knows the name of, so a game whose achievements were never fetched
/// yet (never opened past its library card) would otherwise lose that
/// unlock the moment the log is removed, with no cache row to retry it
/// against later.
async fn apply_hook_log(account_id: &str, game: &InstalledGame, achievement_log: Option<&std::path::Path>) {
    if game.storefront != decanter_core::Storefront::Gog {
        return;
    }
    let Ok(mut db) = decanter_core::Db::open_active() else {
        tracing::debug!(game_id = %game.game_id, "could not open db, decanter-hook log not applied");
        return;
    };
    if let Err(err) = decanter_gog::poll_achievements(&mut db, account_id, &game.game_id).await {
        tracing::warn!(game_id = %game.game_id, %err, "post-session achievement poll failed");
    }
    let Some(path) = achievement_log else {
        return;
    };
    let applied = decanter_gog::hook::process_log(&db, account_id, &game.game_id, path).await;
    if applied > 0 {
        tracing::info!(game_id = %game.game_id, applied, "applied locally-witnessed achievement unlocks");
    }
}

/// Pulls cloud saves down before a session starts, if this game has a
/// folder configured — best-effort, same as [`record_last_played`]: a
/// failure here costs a stale save, not the launch itself.
///
/// Returns the sync checkpoint to classify *this session's* changes
/// against at exit — `game.cloud_save_last_sync` is what it was before
/// this ran, and a successful sync here already moved it forward, so
/// reusing the stale value at exit would tell the classifier a session's
/// worth of files "changed since" a timestamp cloud saves already knows
/// about, and re-push them for nothing.
async fn sync_cloud_saves_before_launch(account_id: &str, game: &InstalledGame) -> Option<i64> {
    let Some(local_root) = game.cloud_save_path.as_deref() else {
        return game.cloud_save_last_sync;
    };
    match decanter_gog::cloud_saves::sync(
        account_id,
        &game.game_id,
        std::path::Path::new(local_root),
        game.cloud_save_last_sync,
        None,
    )
    .await
    {
        Ok(outcome) if outcome.action == decanter_gog::cloud_saves::SyncAction::Conflict => {
            tracing::warn!(
                game_id = %game.game_id,
                "cloud saves conflict before launch, playing with the local copy until resolved"
            );
            game.cloud_save_last_sync
        }
        Ok(outcome) => {
            if let Err(err) = crate::core::record_cloud_save_sync(account_id, &game.game_id, outcome.last_sync) {
                tracing::warn!(game_id = %game.game_id, error = %err, "could not record the cloud saves sync");
            }
            tracing::info!(game_id = %game.game_id, action = ?outcome.action, "cloud saves synced before launch");
            Some(outcome.last_sync)
        }
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, error = %format_args!("{err:#}"), "cloud saves sync before launch failed");
            game.cloud_save_last_sync
        }
    }
}

/// Pushes local changes up once a session ends, mirroring
/// [`sync_cloud_saves_before_launch`]. `checkpoint` is that call's return
/// value, not `game.cloud_save_last_sync` — see its doc for why.
async fn sync_cloud_saves_after_exit(account_id: &str, game: &InstalledGame, checkpoint: Option<i64>) {
    let Some(local_root) = game.cloud_save_path.as_deref() else {
        return;
    };
    match decanter_gog::cloud_saves::sync(
        account_id,
        &game.game_id,
        std::path::Path::new(local_root),
        checkpoint,
        None,
    )
    .await
    {
        Ok(outcome) if outcome.action == decanter_gog::cloud_saves::SyncAction::Conflict => {
            tracing::warn!(game_id = %game.game_id, "cloud saves conflict after exit, left unresolved");
        }
        Ok(outcome) => {
            if let Err(err) = crate::core::record_cloud_save_sync(account_id, &game.game_id, outcome.last_sync) {
                tracing::warn!(game_id = %game.game_id, error = %err, "could not record the cloud saves sync");
            }
            tracing::info!(game_id = %game.game_id, action = ?outcome.action, "cloud saves synced after exit");
        }
        Err(err) => {
            tracing::warn!(game_id = %game.game_id, error = %format_args!("{err:#}"), "cloud saves sync after exit failed");
        }
    }
}

/// Ends a game's session.
pub async fn stop(account_id: String, game_id: String, background: Entity<Background>, cx: &mut AsyncApp) {
    // So the exit this causes reads as a stop in the run's log, not as a crash.
    background.update(cx, |background, cx| {
        if let Some(job) = background.runs.get(&game_id) {
            job.update(cx, |job, _| job.stop_requested = true);
        }
    });
    // By the system's kind, not the game's: a Linux program's session is named
    // after its prefix, so the prefix's stop reaches it too. A Raven program's
    // prefix is Raven's, not the one the system's kind would find.
    if crate::raven::is_program(&account_id, &game_id) {
        return raven::stop(&game_id).await;
    }
    match Kind::host() {
        Kind::Umu | Kind::LinuxNative => umu::stop(&account_id, &game_id).await,
        Kind::Native => native::stop(&account_id, &game_id).await,
        Kind::Raven => raven::stop(&game_id).await,
    }
}

/// Whether `game_id` of `account_id` is still running, which moving its
/// files has to wait for.
pub fn is_running(account_id: &str, game_id: &str) -> bool {
    if crate::raven::is_program(account_id, game_id) {
        return raven::is_running(game_id);
    }
    match Kind::host() {
        Kind::Umu | Kind::LinuxNative => umu::is_running(account_id, game_id),
        Kind::Native => native::is_running(account_id, game_id),
        Kind::Raven => raven::is_running(game_id),
    }
}

/// Stops every game decanter started.
///
/// Games outlive the app that launched them — umu execs into a container
/// and Proton detaches, and a native game is its own process — so closing
/// decanter without this leaves them running with no way left to reach
/// them from the UI.
pub async fn stop_everything() {
    tracing::info!("stopping all running games");
    decanter_runtime::stop_all().await;
}

/// Stops everything and exits, for the signals a desktop actually sends —
/// a session logging out, `pkill`, Ctrl+C from a terminal.
///
/// Only the polite ones: `SIGKILL` can't be caught, so a hard kill still
/// leaves games running. Nothing can change that.
pub async fn exit_with_games_on_signal(cx: &mut AsyncApp) {
    if !crate::platform::exit_signal().await {
        return;
    }
    stop_everything().await;

    // Not `std::process::exit` -- that tears the process down while other
    // tokio worker threads (a still-in-flight GOG library fetch, say) may
    // still be inside OpenSSL, racing its own `atexit` cleanup and
    // segfaulting in `OPENSSL_cleanup` (confirmed via coredumpctl: SIGSEGV
    // in `ossl_lib_ctx_default_deinit`, concurrent with a `spawn_blocking`
    // GOG session call). Quitting GPUI instead returns from `main`'s run
    // loop, and the tokio runtime `main` owns is then dropped the ordinary
    // way -- which blocks until outstanding `spawn_blocking` work finishes,
    // so nothing is still touching OpenSSL by the time the process exits.
    cx.update(|cx| cx.quit());
}

/// Stamps the manifest with when the session ended. Best-effort: a failed
/// write costs a sort order, not a game.
fn record_last_played(account_id: &str, game: &InstalledGame, kind: Kind) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64);
    if kind == Kind::Raven {
        return crate::raven::record_last_played(game, now);
    }
    let Ok(library) = crate::core::library_for_account(account_id) else {
        tracing::debug!(game_id = %game.game_id, "could not record last played, library unavailable");
        return;
    };
    // Re-read rather than reusing the copy this launch started from: the
    // session may have outlasted an edit to the manifest.
    let mut current = library
        .read_manifest(account_id, game.storefront, &game.game_id)
        .unwrap_or_else(|| game.clone());
    current.last_played = Some(now);
    if let Err(err) = library.write_manifest(&current) {
        tracing::warn!(game_id = %game.game_id, %err, "could not record last played");
    }
}
