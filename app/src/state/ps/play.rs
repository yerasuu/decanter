//! Playing a game: its port made ready first, a PS5 game converted first,
//! then the game run with its output scanned for failures, which go to the
//! profile's own record.

use std::io;
use std::path::{Path, PathBuf};

use decanter_core::capabilities::Feature;
use decanter_core::{KeyringError, ShadnetSecret};
use decanter_ps::anyps5::{self, AnyPs5};
use decanter_ps::bbport::GameLaunch;
use decanter_ps::bbport::icons::IconStyle;
use decanter_ps::bbport::mods::Mods;
use decanter_ps::bbport::online::{self, Online, OnlineError};
use decanter_ps::bbport::{self, Bbport, Ini, LauncherSettings};
use decanter_ps::failures::{self, Failure, Scanner, Summary};
use decanter_ps::learned;
use decanter_ps::library::Game;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::{self, JobKind};
use decanter_ps::toolchain::Toolchain;
use futures::future::Shared;
use gpui::{AppContext as _, AsyncApp, Context, Task, WeakEntity};
use tokio::process::Command;

use super::Ps;
use super::jobs::{self, JobKey, Launcher};
use super::ports::Prepared;
use super::vk_layer;
use crate::capabilities::Caps;
use crate::i18n::Text;

impl Ps {
    pub fn is_running(&self, key: &JobKey, cx: &gpui::App) -> bool {
        self.jobs.get(key).is_some_and(|job| job.read(cx).is_running())
    }

    pub fn play(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let key = JobKey::Game(dir.to_owned());
        if self.is_running(&key, cx) {
            return;
        }
        if self.moving.contains(dir) {
            self.notice = Some((Text::new("ps.play.moving"), true));
            cx.notify();
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
        let folder = self
            .library_of(dir)
            .map_or_else(|| root.library().to_owned(), |(folder, _)| folder.to_owned());
        let failures_path = root.profile(&profile_id).join("failures.jsonl");
        // Without the toolchain's builds a port runs only from a folder the user picked.
        let prepared = Caps::get(cx)
            .is_offered(Feature::PsBuilds)
            .then(|| self.prepare(port, &root, cx));
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let cache_dir = game_cache_dir(folder, &game.title_id, cx).await;
            let started = match prepared {
                Some(prepared) => {
                    let cache_dir = cache_dir.as_deref();
                    through_toolchain(&this, &root, port, &game, &profile_id, cache_dir, prepared, cx).await
                }
                #[cfg(windows)]
                None => super::direct::launch(&root, port, &game, &profile_id, cache_dir.as_deref(), cx).await,
                #[cfg(not(windows))]
                None => Err(Text::new("ps.errors.builds_unavailable")),
            };
            let (launcher, facts) = match started {
                Ok(started) => started,
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
            let finished = jobs::run_launcher(
                &this,
                &root,
                JobKey::Game(game.dir.clone()),
                JobKind::Game,
                facts,
                launcher,
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
        let Some(job) = self.jobs.get(key) else {
            return;
        };
        #[cfg(windows)]
        if let JobKey::Game(dir) = key {
            super::direct::stop(job, dir, cx);
            return;
        }
        job.update(cx, |job, cx| {
            job.stop();
            cx.notify();
        });
    }

    /// Builds `<base>-01.09` beside the base dump, then shows it.
    pub fn merge(&mut self, base: PathBuf, update: PathBuf, cx: &mut Context<Self>) {
        if self.moving.contains(&base) || self.moving.contains(&update) {
            self.notice = Some((Text::new("ps.play.moving"), true));
            cx.notify();
            return;
        }
        self.notice = Some((Text::new("ps.play.merging"), false));
        cx.notify();
        let task = cx.background_spawn(async move { bbport::merge_update(&base, &update, bbport::SUPPORTED_VERSION) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.notice = Some(match result {
                    Ok(merged) => (Text::new("ps.play.merged").arg("dir", merged.display()), false),
                    Err(err) => (Text::new("ps.play.merge_error").arg("error", err), true),
                });
                this.rescan(cx);
            });
        })
        .detach();
    }
}

/// The port prepared in its toolchain, then the game's command in it with what a run of the port
/// adds around it: the validation layer, MangoHud, GameMode and a scope of its own.
#[expect(
    clippy::too_many_arguments,
    reason = "the launch's own inputs plus the preparation already under way"
)]
async fn through_toolchain(
    this: &WeakEntity<Ps>,
    root: &Root,
    port: Port,
    game: &Game,
    profile_id: &str,
    cache_dir: Option<&Path>,
    prepared: Shared<Task<Prepared>>,
    cx: &mut AsyncApp,
) -> Result<(Launcher, Vec<(&'static str, String)>), Text> {
    let toolchain = match prepared.await {
        Ok(toolchain) => toolchain,
        Err(err) => {
            return Err(Text::new("ps.play.prepare_error")
                .arg("name", port.name())
                .text("error", err));
        }
    };
    let command = match port {
        Port::Bbport => bbport_command(root, &toolchain, game, profile_id, cache_dir, cx).await,
        Port::AnyPs5 => anyps5_command(this, root, &toolchain, game, profile_id, cache_dir, cx).await,
    };
    let command = command.map_err(|err| {
        tracing::warn!(port = port.name(), title_id = %game.title_id, error = %err, "could not start game");
        err.localized()
    });
    let facts = jobs::port_facts(root, port, Some(&toolchain)).await;
    let (command, facts) = vk_layer::apply(this, root, &toolchain, &game.dir, command, facts, cx).await;
    let mut command = match command {
        Ok(command) => command,
        Err(err) => return Err(err),
    };
    let key = game.key();
    let (mangohud, gamemoded) = crate::core::db()
        .map(|db| {
            (
                crate::game_tools::mangohud(&db, &key),
                crate::game_tools::gamemoded(&db, &key),
            )
        })
        .unwrap_or_default();
    if let Some(autostart_log) = mangohud {
        command.envs(crate::game_tools::mangohud_env(&key, root.library(), autostart_log).await);
    }
    if let Some(gamemoded) = gamemoded {
        command = process::through_gamemode(command, &gamemoded);
    }
    Ok((
        Launcher::Command(process::in_own_scope(command, &key, &game.title)),
        facts,
    ))
}

/// Why a game could not be started: logged as it is, worded for the UI by
/// [`LaunchError::localized`].
#[derive(Debug, thiserror::Error)]
pub(super) enum LaunchError {
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
    #[error("online settings unusable: {0}")]
    Online(OnlineError),
    /// Asking for the password at each launch instead is left for later: the launch refuses.
    #[error("shadNet secrets unreadable: {0}")]
    Keyring(String),
    #[error("no shadNet password in the keyring for {0}")]
    NoPassword(String),
}

impl LaunchError {
    pub(super) fn localized(&self) -> Text {
        match self {
            Self::Mods(err) => Text::new("ps.play.mods_error").arg("error", err),
            Self::CreateDir { dir, err } => Text::new("ps.errors.create_dir")
                .arg("dir", dir.display())
                .arg("error", err),
            Self::Environment(err) => Text::new("ps.errors.environment").arg("error", err),
            Self::ConvertPrepare(err) => Text::new("ps.play.convert_prepare_error").arg("error", err),
            Self::ConvertFailed => Text::new("ps.play.convert_failed"),
            Self::RunDir { dir, err } => Text::new("ps.play.prepare_error")
                .arg("name", dir.display())
                .arg("error", err),
            Self::Online(err) => Text::new("ps.play.online_error").text("error", online_error(err)),
            Self::Keyring(err) => Text::new("ps.play.online_keyring_error").arg("error", err),
            Self::NoPassword(npid) => Text::new("ps.play.online_no_password").arg("npid", npid),
        }
    }
}

fn notify(this: &WeakEntity<Ps>, text: Text, error: bool, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.notice = Some((text, error));
        cx.notify();
    });
}

/// Everything a bbport launch reads besides the game and its cache folder, gathered before the
/// launch borrows it ([`LaunchInputs::game_launch`]).
pub(super) struct LaunchInputs {
    pub(super) settings: LauncherSettings,
    learned: learned::Title,
    aspect_ratio: Option<&'static str>,
    mods: Mods,
    debug_dir: Option<PathBuf>,
    user_dir: PathBuf,
    online: Option<Online>,
    menu_language: String,
}

impl LaunchInputs {
    pub(super) fn game_launch<'inputs>(
        &'inputs self,
        game: &'inputs Game,
        cache_dir: Option<&'inputs Path>,
    ) -> GameLaunch<'inputs> {
        GameLaunch {
            game,
            user_dir: &self.user_dir,
            cache_dir,
            debug_dir: self.debug_dir.as_deref(),
            settings: &self.settings,
            mods: &self.mods,
            menu_language: &self.menu_language,
            learned: &self.learned,
            aspect_ratio: self.aspect_ratio,
            online: self.online.as_ref(),
        }
    }
}

/// `toolchain` only tells the connected pads apart for the layout's button icons; without one the
/// icons take their automatic style's default.
pub(super) async fn launch_inputs(
    root: &Root,
    toolchain: Option<&Toolchain>,
    game: &Game,
    profile_id: &str,
    cx: &mut AsyncApp,
) -> Result<LaunchInputs, LaunchError> {
    let (settings, learned, chosen_ratio) = {
        let root = root.clone();
        let title_id = game.title_id.clone();
        cx.background_spawn(async move {
            (
                LauncherSettings::load(&root),
                learned::Store::load(&root).title(&title_id),
                Ini::load(&Bbport::ini_path(&root))
                    .get("aspect_ratio")
                    .map(str::to_owned),
            )
        })
        .await
    };
    let aspect_ratio = match chosen_ratio.as_deref() {
        None | Some("auto") => display_aspect_ratio(cx),
        Some(_) => None,
    };
    let mods = if bbport::is_bloodborne(game) {
        // The automatic icon style follows the pad bbport will read: the one
        // picked in Mando, else the first connected.
        let detected = match toolchain {
            Some(toolchain) => {
                let pads = Bbport::new(root.clone()).connected_gamepads(toolchain).await;
                let chosen = settings.text("gamepad");
                pads.iter()
                    .find(|(guid, _)| Some(guid.as_str()) == chosen)
                    .or_else(|| pads.first())
                    .map(|(guid, name)| IconStyle::for_gamepad(guid, name))
            }
            None => None,
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
        Mods::off(root, &settings)
    };
    let debug_dir = crate::debug_logs::ps_folder(settings.title_flag(&game.title_id, crate::debug_logs::TITLE_KEY));
    let user_dir = root.profile(profile_id).join("bbport/user");
    if let Err(err) = tokio::fs::create_dir_all(&user_dir).await {
        return Err(LaunchError::CreateDir { dir: user_dir, err });
    }
    let online = match online_play(&settings, cx).await {
        Ok(online) => online,
        Err(err) => return Err(err),
    };
    Ok(LaunchInputs {
        settings,
        learned,
        aspect_ratio,
        mods,
        debug_dir,
        user_dir,
        online,
        menu_language: rust_i18n::locale().to_string(),
    })
}

async fn bbport_command(
    root: &Root,
    toolchain: &Toolchain,
    game: &Game,
    profile_id: &str,
    cache_dir: Option<&Path>,
    cx: &mut AsyncApp,
) -> Result<Command, LaunchError> {
    let inputs = match launch_inputs(root, Some(toolchain), game, profile_id, cx).await {
        Ok(inputs) => inputs,
        Err(err) => return Err(err),
    };
    Bbport::new(root.clone())
        .launch_command(toolchain, &inputs.game_launch(game, cache_dir))
        .await
        .map_err(LaunchError::Environment)
}

/// Online play when it is on and the build has it, with the account's secrets from the keyring.
async fn online_play(settings: &LauncherSettings, cx: &AsyncApp) -> Result<Option<Online>, LaunchError> {
    if !settings.flag(online::ONLINE, false) {
        return Ok(None);
    }
    if !cx.update(|cx| Caps::get(cx).is_offered(Feature::PsOnline)) {
        tracing::warn!("online play is on, but this psport build has no online module: playing offline");
        return Ok(None);
    }
    let mut online = match Online::from_settings(settings) {
        Ok(Some(online)) => online,
        Ok(None) => return Ok(None),
        Err(err) => return Err(LaunchError::Online(err)),
    };
    let Some(npid) = online.npid().map(str::to_owned) else {
        tracing::info!("online play without a shadNet account");
        return Ok(Some(online));
    };
    let secrets = tokio::task::spawn_blocking(move || {
        let password = decanter_core::load_shadnet(&npid, ShadnetSecret::Password)?;
        let token = decanter_core::load_shadnet(&npid, ShadnetSecret::Token)?;
        Ok::<_, KeyringError>((password, token))
    })
    .await;
    let (password, token) = match secrets {
        Ok(Ok(secrets)) => secrets,
        Ok(Err(err)) => return Err(LaunchError::Keyring(err.to_string())),
        Err(err) => return Err(LaunchError::Keyring(err.to_string())),
    };
    if password.is_none() {
        tracing::warn!("online play with a shadNet NPID but no password in the keyring, not starting");
        return Err(LaunchError::NoPassword(online.npid().unwrap_or_default().to_owned()));
    }
    match online.set_secrets(password, token) {
        Ok(()) => Ok(Some(online)),
        Err(err) => Err(LaunchError::Online(err)),
    }
}

/// The ratio of the display the game opens on. decanter sets no `PS_DISPLAY`, so the port takes
/// SDL's primary display.
fn display_aspect_ratio(cx: &mut AsyncApp) -> Option<&'static str> {
    let size = cx.update(|cx| {
        // Wayland names no primary display; compositors open the game on the focused output,
        // decanter's while Play is clicked.
        cx.primary_display()
            .or_else(|| {
                cx.active_window()?
                    .update(cx, |_, window, cx| window.display(cx))
                    .ok()
                    .flatten()
            })
            .or_else(|| cx.displays().into_iter().next())
            .map(|display| display.bounds().size)
    });
    let Some(size) = size else {
        tracing::warn!("no display to take the aspect ratio from, the port keeps its own");
        return None;
    };
    let (width, height) = (f32::from(size.width), f32::from(size.height));
    let ratio = bbport::nearest_aspect_ratio(width, height);
    tracing::info!(width, height, ratio, "display aspect ratio for the port");
    ratio
}

/// Converts first when the game has no conversion yet; the conversion is
/// shared, the run directory is the profile's.
#[expect(
    clippy::too_many_arguments,
    reason = "the game's cache folder joins what the conversion and the run already took"
)]
async fn anyps5_command(
    this: &WeakEntity<Ps>,
    root: &Root,
    toolchain: &Toolchain,
    game: &Game,
    profile_id: &str,
    cache_dir: Option<&Path>,
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
    let mut command = match anyps5.run_command(toolchain, game, &run_dir).await {
        Ok(command) => command,
        Err(err) => return Err(LaunchError::Environment(err)),
    };
    // AnyPS5 has no cache of its own to root; only the drivers' follow the game.
    if let Some(dir) = cache_dir {
        command.envs(decanter_core::shader_cache_env(dir));
    }
    Ok(command)
}

/// The game's cache folder in the cache location of `folder`, the library
/// folder holding its dump.
async fn game_cache_dir(folder: PathBuf, title_id: &str, cx: &AsyncApp) -> Option<PathBuf> {
    let title_id = title_id.to_owned();
    cx.background_spawn(async move { crate::cache::game_dir(&folder, "ps", &title_id) })
        .await
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
            notify(this, Text::new("ps.learned.save_error").arg("error", err), true, cx);
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
        .map(failure_message);
    let count = found
        .iter()
        .filter(|failure| failure.kind == failures::Kind::Error)
        .count();
    let missing = found
        .iter()
        .find(|failure| failure.kind == failures::Kind::Missing)
        .map(failure_message);
    let saved = cx
        .background_spawn(async move {
            let mut log = failures::Log::load(path);
            log.append(found)
        })
        .await;
    let (text, error) = match (saved, fatal) {
        (Err(err), _) => (Text::new("ps.failures.notice.save_error").arg("error", err), true),
        (Ok(()), Some(message)) => (Text::new("ps.failures.notice.crashed").text("message", message), true),
        (Ok(()), None) => match missing {
            Some(missing) if count == 0 => (Text::new("ps.failures.notice.missing").text("missing", missing), false),
            Some(missing) if count == 1 => (
                Text::new("ps.failures.notice.errors_and_missing.one")
                    .arg("count", count)
                    .text("missing", missing),
                false,
            ),
            Some(missing) => (
                Text::new("ps.failures.notice.errors_and_missing.other")
                    .arg("count", count)
                    .text("missing", missing),
                false,
            ),
            None if count == 1 => (Text::new("ps.failures.notice.errors.one").arg("count", count), false),
            None => (Text::new("ps.failures.notice.errors.other").arg("count", count), false),
        },
    };
    let _ = this.update(cx, |this, cx| {
        this.notice = Some((text, error));
        this.failures_changed(cx);
    });
}

/// Why an online setting can't be used, in the UI's language.
pub fn online_error(err: &OnlineError) -> Text {
    match err {
        OnlineError::Server(value) => Text::new("ps.online.errors.server").arg("value", value),
        OnlineError::Community(value) => Text::new("ps.online.errors.community").arg("value", value),
        OnlineError::Webapi(value) => Text::new("ps.online.errors.webapi").arg("value", value),
        OnlineError::NpidEmail => Text::new("ps.online.errors.npid_email"),
        OnlineError::Npid(value) => Text::new("ps.online.errors.npid").arg("value", value),
        OnlineError::Secret => Text::new("ps.online.errors.secret"),
    }
}

/// A failure's message in the UI's language when decanter worded it, as the port printed it
/// otherwise.
pub fn failure_message(failure: &Failure) -> Text {
    match failure.summary {
        Some(Summary::Missing { total: 1, called }) => Text::new("ps.failures.missing.one")
            .arg("total", 1)
            .arg("called", called),
        Some(Summary::Missing { total, called }) => Text::new("ps.failures.missing.other")
            .arg("total", total)
            .arg("called", called),
        Some(Summary::Calls {
            unimplemented,
            stubs,
            failed,
            status,
        }) => Text::new("ps.failures.calls.summary")
            .arg("unimplemented", unimplemented)
            .arg("stubs", stubs)
            .arg("failed", failed)
            .arg("status", status),
        Some(Summary::Exit(Some(code))) => Text::new("ps.failures.exit_code").arg("code", code),
        Some(Summary::Exit(None)) => Text::new("ps.failures.exit_signal"),
        None => Text::verbatim(&failure.message),
    }
}
