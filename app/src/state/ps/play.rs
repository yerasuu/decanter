//! Playing a game: its port made ready first, a PS5 game converted first,
//! then the game run with its output scanned for failures, which go to the
//! profile's own record.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

use decanter_ps::anyps5::{self, AnyPs5};
use decanter_ps::bbport::icons::IconStyle;
use decanter_ps::bbport::{self, Bbport, LauncherSettings};
use decanter_ps::failures::{self, Failure, Scanner, Summary};
use decanter_ps::learned;
use decanter_ps::library::Game;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::{self, JobKind};
use decanter_ps::toolchain::Toolchain;
use gpui::{AppContext as _, AsyncApp, Context, WeakEntity};
use rust_i18n::t;
use tokio::process::Command;

use super::Ps;
use super::jobs::{self, JobKey};

impl Ps {
    pub fn is_running(&self, key: &JobKey, cx: &gpui::App) -> bool {
        self.jobs.get(key).is_some_and(|job| job.read(cx).is_running())
    }

    pub fn play(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let key = JobKey::Game(dir.to_owned());
        if self.is_running(&key, cx) {
            return;
        }
        let Some(entry) = self.entries.iter().find(|entry| entry.game.dir == dir) else {
            return;
        };
        let (Some(port), Some(profile_id)) = (entry.port, self.profile_id.clone()) else {
            return;
        };
        if !self.launching.insert(dir.to_owned()) {
            return;
        }
        let game = entry.game.clone();
        let root = decanter_ps::paths::active();
        let failures_path = root.profile(&profile_id).join("failures.jsonl");
        let prepared = self.prepare(port, &root, cx);
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (command, facts) = match prepared.await {
                Ok(toolchain) => {
                    let command = match port {
                        Port::Bbport => bbport_command(&root, &toolchain, &game, &profile_id, cx).await,
                        Port::AnyPs5 => anyps5_command(&this, &root, &toolchain, &game, &profile_id, cx).await,
                    };
                    let command = command.map_err(|err| {
                        tracing::warn!(port = port.name(), title_id = %game.title_id, error = %err, "could not start game");
                        err.localized()
                    });
                    (command, jobs::port_facts(&root, port, Some(&toolchain)).await)
                }
                Err(err) => (
                    Err(t!("ps.play.prepare_error", name = port.name(), error = err).into_owned()),
                    Vec::new(),
                ),
            };
            let command = match command {
                Ok(command) => process::in_own_scope(command, &game.key(), &game.title),
                Err(err) => {
                    let _ = this.update(cx, |this, _| this.launching.remove(&game.dir));
                    return notify(&this, err, true, cx);
                }
            };
            // Before the window can appear, so the task bar already knows it.
            crate::taskbar::sync_ps(root.library(), &game).await;
            let commit = failures::commit_of(&port.checkout(&root)).await;
            let scanner = Scanner::new(&game.dir, &game.title_id, &game.version, commit);
            let target = crate::closing::Target::Ps(game.dir.clone());
            cx.update(|cx| crate::closing::started(target.clone(), game.title.clone(), cx));
            let finished = jobs::run(
                &this,
                &root,
                JobKey::Game(game.dir.clone()),
                JobKind::Game,
                facts,
                command,
                Some(scanner),
                cx,
            )
            .await;
            cx.update(|cx| crate::closing::ended(&target, cx));
            let _ = this.update(cx, |this, _| this.launching.remove(&game.dir));
            if let Some(line) = finished.frame_overlap_off {
                record_frame_overlap_off(&this, &root, &game.title_id, line, cx).await;
            }
            record_failures(&this, failures_path, finished.failures, cx).await;
        })
        .detach();
    }

    pub fn stop(&mut self, key: &JobKey, cx: &mut Context<Self>) {
        if let Some(job) = self.jobs.get(key) {
            job.update(cx, |job, cx| {
                job.stop();
                cx.notify();
            });
        }
    }

    /// Builds `<base>-01.09` beside the base dump, then shows it.
    pub fn merge(&mut self, base: PathBuf, update: PathBuf, cx: &mut Context<Self>) {
        self.notice = Some((t!("ps.play.merging").into_owned(), false));
        cx.notify();
        let task = cx.background_spawn(async move { bbport::merge_update(&base, &update, bbport::SUPPORTED_VERSION) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.notice = Some(match result {
                    Ok(merged) => (t!("ps.play.merged", dir = merged.display()).into_owned(), false),
                    Err(err) => (t!("ps.play.merge_error", error = err).into_owned(), true),
                });
                this.rescan(cx);
            });
        })
        .detach();
    }
}

/// Why a game could not be started: logged as it is, worded for the UI by
/// [`LaunchError::localized`].
#[derive(Debug, thiserror::Error)]
enum LaunchError {
    #[error("mods setup failed: {0}")]
    Mods(io::Error),
    #[error("could not create {}: {err}", dir.display())]
    CreateDir { dir: PathBuf, err: io::Error },
    #[error("launch environment setup failed: {0}")]
    Environment(io::Error),
    #[error("conversion output setup failed: {0}")]
    ConvertPrepare(io::Error),
    #[error("conversion failed")]
    ConvertFailed,
    #[error("run directory setup failed at {}: {err}", dir.display())]
    RunDir { dir: PathBuf, err: io::Error },
}

impl LaunchError {
    fn localized(&self) -> String {
        match self {
            Self::Mods(err) => t!("ps.play.mods_error", error = err),
            Self::CreateDir { dir, err } => t!("ps.errors.create_dir", dir = dir.display(), error = err),
            Self::Environment(err) => t!("ps.errors.environment", error = err),
            Self::ConvertPrepare(err) => t!("ps.play.convert_prepare_error", error = err),
            Self::ConvertFailed => t!("ps.play.convert_failed"),
            Self::RunDir { dir, err } => t!("ps.play.prepare_error", name = dir.display(), error = err),
        }
        .into_owned()
    }
}

fn notify(this: &WeakEntity<Ps>, text: String, error: bool, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.notice = Some((text, error));
        cx.notify();
    });
}

async fn bbport_command(
    root: &Root,
    toolchain: &Toolchain,
    game: &Game,
    profile_id: &str,
    cx: &mut AsyncApp,
) -> Result<Command, LaunchError> {
    let (settings, learned) = {
        let root = root.clone();
        let title_id = game.title_id.clone();
        cx.background_spawn(async move {
            (
                LauncherSettings::load(&root),
                learned::Store::load(&root).title(&title_id),
            )
        })
        .await
    };
    let bbport = Bbport::new(root.clone());
    let mods = if bbport::is_bloodborne(game) {
        // The automatic icon style follows the pad bbport will read: the one
        // picked in Mando, else the first connected.
        let detected = {
            let pads = bbport.connected_gamepads(toolchain).await;
            let chosen = settings.text("gamepad");
            pads.iter()
                .find(|(guid, _)| Some(guid.as_str()) == chosen)
                .or_else(|| pads.first())
                .map(|(guid, name)| IconStyle::for_gamepad(guid, name))
        };
        let game_dir = game.dir.clone();
        let for_mods = settings.clone();
        let root = root.clone();
        // The layout's button icons are built here when they need to be: a few
        // hundred milliseconds of decoding and encoding, off the UI thread.
        let mods = cx
            .background_spawn(async move { bbport::mods::prepare(&root, &game_dir, &for_mods, detected) })
            .await;
        match mods {
            Ok(mods) => mods,
            Err(err) => return Err(LaunchError::Mods(err)),
        }
    } else {
        bbport::mods::Mods::off(root, &settings)
    };
    let user_dir = root.profile(profile_id).join("bbport/user");
    if let Err(err) = tokio::fs::create_dir_all(&user_dir).await {
        return Err(LaunchError::CreateDir { dir: user_dir, err });
    }
    bbport
        .launch_command(
            toolchain,
            game,
            &user_dir,
            &settings,
            &mods,
            &rust_i18n::locale(),
            &learned,
        )
        .await
        .map_err(LaunchError::Environment)
}

/// Converts first when the game has no conversion yet; the conversion is
/// shared, the run directory is the profile's.
async fn anyps5_command(
    this: &WeakEntity<Ps>,
    root: &Root,
    toolchain: &Toolchain,
    game: &Game,
    profile_id: &str,
    cx: &mut AsyncApp,
) -> Result<Command, LaunchError> {
    let anyps5 = AnyPs5::new(root.clone());
    if !anyps5.is_converted(&game.title_id).await {
        let prepared = {
            let root = root.clone();
            let game = game.clone();
            cx.background_spawn(async move { AnyPs5::new(root).prepare_output(&game) })
                .await
        };
        if let Err(err) = prepared {
            return Err(LaunchError::ConvertPrepare(err));
        }
        let command = match anyps5.convert_command(toolchain, game).await {
            Ok(command) => command,
            Err(err) => return Err(LaunchError::Environment(err)),
        };
        let mut facts = jobs::port_facts(root, Port::AnyPs5, Some(toolchain)).await;
        facts.extend([
            ("title_id", game.title_id.clone()),
            ("game_dir", game.dir.display().to_string()),
        ]);
        let finished = jobs::run(
            this,
            root,
            JobKey::Game(game.dir.clone()),
            JobKind::Convert,
            facts,
            command,
            None,
            cx,
        )
        .await;
        if !finished.succeeded() {
            return Err(LaunchError::ConvertFailed);
        }
    }
    let run_dir = root.profile(profile_id).join("anyps5").join(&game.title_id);
    let laid_out = {
        let title_id = game.title_id.clone();
        let run_dir = run_dir.clone();
        let root = root.clone();
        cx.background_spawn(async move { anyps5::prepare_run_dir(&AnyPs5::new(root), &title_id, &run_dir) })
            .await
    };
    if let Err(err) = laid_out {
        return Err(LaunchError::RunDir { dir: run_dir, err });
    }
    anyps5
        .run_command(toolchain, game, &run_dir)
        .await
        .map_err(LaunchError::Environment)
}

/// The port caught the game rewriting frames in flight and stopped overlapping them; the next
/// run starts that way, so its first frames are not corrupted while the guard catches it again.
async fn record_frame_overlap_off(this: &WeakEntity<Ps>, root: &Root, title_id: &str, line: String, cx: &mut AsyncApp) {
    tracing::info!(
        title_id,
        line,
        "the port turned frame overlap off: next runs start without it"
    );
    let saved = {
        let (root, title_id) = (root.clone(), title_id.to_owned());
        cx.background_spawn(async move { learned::Store::record_frame_overlap_off(&root, &title_id, line) })
            .await
    };
    match saved {
        Ok(()) => {
            let _ = this.update(cx, Ps::learned_changed);
        }
        Err(err) => {
            tracing::warn!(title_id, %err, "could not record that frame overlap is off for the title");
            notify(this, t!("ps.learned.save_error", error = err).into_owned(), true, cx);
        }
    }
}

async fn record_failures(this: &WeakEntity<Ps>, path: PathBuf, found: Vec<Failure>, cx: &mut AsyncApp) {
    if found.is_empty() {
        return;
    }
    let fatal = found
        .iter()
        .find(|failure| {
            matches!(
                failure.kind,
                failures::Kind::Crash | failures::Kind::Stop | failures::Kind::Exit
            )
        })
        .map(|failure| failure_message(failure).into_owned());
    let count = found
        .iter()
        .filter(|failure| failure.kind == failures::Kind::Error)
        .count();
    let missing = found
        .iter()
        .find(|failure| failure.kind == failures::Kind::Missing)
        .map(|failure| failure_message(failure).into_owned());
    let saved = cx
        .background_spawn(async move {
            let mut log = failures::Log::load(path);
            log.append(found)
        })
        .await;
    let (text, error) = match (saved, fatal) {
        (Err(err), _) => (t!("ps.failures.notice.save_error", error = err), true),
        (Ok(()), Some(message)) => (t!("ps.failures.notice.crashed", message = message), true),
        (Ok(()), None) => match missing {
            Some(missing) if count == 0 => (t!("ps.failures.notice.missing", missing = missing), false),
            Some(missing) if count == 1 => (
                t!(
                    "ps.failures.notice.errors_and_missing.one",
                    count = count,
                    missing = missing
                ),
                false,
            ),
            Some(missing) => (
                t!(
                    "ps.failures.notice.errors_and_missing.other",
                    count = count,
                    missing = missing
                ),
                false,
            ),
            None if count == 1 => (t!("ps.failures.notice.errors.one", count = count), false),
            None => (t!("ps.failures.notice.errors.other", count = count), false),
        },
    };
    let text = text.into_owned();
    let _ = this.update(cx, |this, cx| {
        this.notice = Some((text, error));
        this.failures_changed(cx);
    });
}

/// A failure's message in the UI's language when decanter worded it, as the port printed it
/// otherwise.
pub fn failure_message(failure: &Failure) -> Cow<'_, str> {
    match failure.summary {
        Some(Summary::Missing { total: 1, called }) => t!("ps.failures.missing.one", total = 1, called = called),
        Some(Summary::Missing { total, called }) => t!("ps.failures.missing.other", total = total, called = called),
        Some(Summary::Calls {
            unimplemented,
            stubs,
            failed,
            status,
        }) => t!(
            "ps.failures.calls.summary",
            unimplemented = unimplemented,
            stubs = stubs,
            failed = failed,
            status = status
        ),
        Some(Summary::Exit(Some(code))) => t!("ps.failures.exit_code", code = code),
        Some(Summary::Exit(None)) => t!("ps.failures.exit_signal"),
        None => Cow::Borrowed(&failure.message),
    }
}
