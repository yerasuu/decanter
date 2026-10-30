//! Playing a game: its port made ready first, a PS5 game converted first,
//! then the game run with its output scanned for failures, which go to the
//! profile's own record.

use std::path::{Path, PathBuf};

use decanter_core::capabilities::Feature;
use decanter_nix::Toolchain;
use decanter_ps::anyps5::AnyPs5;
use decanter_ps::bbport::online;
use decanter_ps::bbport::{self, LauncherSettings};
use decanter_ps::failures::{self, Failure, Scanner, Summary};
use decanter_ps::library::Game;
use decanter_ps::paths::Root;
use decanter_ps::port::Port;
use decanter_ps::process::JobKind;
use futures::future::Shared;
use gpui::{AppContext as _, AsyncApp, Context, Task, WeakEntity};
use tokio::process::Command;

use super::Ps;
use super::jobs::{self, JobKey, Launcher};
use super::ports::Prepared;
use super::vk_layer;
use crate::capabilities::Caps;
use crate::i18n::Text;
use crate::ps_play::launch::{self, LaunchError, OnlinePlay, Surroundings};
use crate::ps_play::run;

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
        let failures_path = run::failures_path(&root, &profile_id);
        // Where psport.exe runs directly, or without the checkouts' builds, a port runs from a folder,
        // a package or a Windows build.
        let prepared = (!decanter_ps::bbport::direct::IN_USE && Caps::get(cx).is_offered(Feature::PsBuilds))
            .then(|| self.prepare(port, &root, cx));
        self.notice = None;
        self.run_notices.remove(dir);
        cx.notify();
        let dir = game.dir.clone();
        let task = cx.spawn(async move |this, cx| {
            let cache_dir = launch::game_cache_dir(folder, &game.title_id).await;
            let started = match prepared {
                Some(prepared) => {
                    let cache_dir = cache_dir.as_deref();
                    through_toolchain(&this, &root, port, &game, &profile_id, cache_dir, prepared, cx).await
                }
                #[cfg(windows)]
                None => {
                    let request = super::direct::Request {
                        root: &root,
                        port,
                        game: &game,
                        profile_id: &profile_id,
                        cache_dir: cache_dir.as_deref(),
                    };
                    super::direct::launch(&this, request, cx).await
                }
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
            let is_online = {
                let (root, title_id) = (root.clone(), game.title_id.clone());
                cx.background_spawn(async move {
                    LauncherSettings::of_title(&root, &title_id)
                        .is_ok_and(|settings| settings.flag(online::ONLINE, false))
                })
                .await
            } && cx.update(|cx| Caps::get(cx).is_offered(Feature::PsOnline));
            let scanner = Scanner::new(&game.dir, &game.title_id, &game.version, commit, !is_online);
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
            if let Some(frame_gen) = finished.frame_gen {
                run::save_frame_gen(&root, &game.title_id, frame_gen).await;
            }
            record_failures(&this, game.dir.clone(), failures_path, finished.failures, cx).await;
        });
        self.launch_tasks.insert(dir, task);
    }

    /// Whether a launch is under way: preparing the port, building the command or starting the
    /// process, which DETENER can end from the first of those on.
    pub fn is_launching(&self, dir: &Path) -> bool {
        // Where psport.exe runs directly, DETENER goes through the session's stop file instead.
        cfg!(not(windows)) && self.launching.contains(dir)
    }

    /// DETENER before the game's process exists: ends the launch's own task and stops the port
    /// job (a clone, a build) it may be waiting on, so nothing is left running behind it.
    #[cfg(not(windows))]
    fn cancel_launch(&mut self, dir: &Path, cx: &mut Context<Self>) {
        self.launch_tasks.remove(dir);
        self.launching.remove(dir);
        let port = self
            .entries
            .iter()
            .find(|entry| entry.game.dir == dir)
            .and_then(|entry| entry.port);
        if let Some(port) = port {
            self.stop(&JobKey::Port(port), cx);
        }
        crate::closing::ended(&crate::closing::Target::Ps(dir.to_owned()), cx);
        tracing::info!(dir = %dir.display(), "launch stopped before the game started");
        cx.notify();
    }

    pub fn is_stopping(&self, dir: &Path) -> bool {
        self.stopping.contains(dir)
    }

    pub fn stop(&mut self, key: &JobKey, cx: &mut Context<Self>) {
        #[cfg(windows)]
        if let JobKey::Game(dir) = key {
            super::direct::stop(self, dir, super::direct::Ending::Polite, cx);
            return;
        }
        let Some(job) = self.jobs.get(key) else {
            return;
        };
        job.update(cx, |job, cx| {
            job.stop();
            cx.notify();
        });
    }

    /// DETENER: where `psport.exe` runs in a session of its own, it is asked through its stop file instead
    /// of its window, and the session ended if it hasn't after the grace; elsewhere the same as [`Ps::stop`].
    pub fn end(&mut self, key: &JobKey, cx: &mut Context<Self>) {
        #[cfg(not(windows))]
        if let JobKey::Game(dir) = key
            && self.launching.contains(dir)
            && !self.is_running(key, cx)
        {
            self.cancel_launch(dir, cx);
            return;
        }
        #[cfg(windows)]
        if let JobKey::Game(dir) = key {
            super::direct::stop(self, dir, super::direct::Ending::Now, cx);
            return;
        }
        self.stop(key, cx);
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
    let (command, secrets) = match port {
        Port::Bbport => {
            let surroundings = surroundings(cx);
            match launch::bbport_command(root, &toolchain, game, profile_id, cache_dir, surroundings).await {
                Ok((command, secrets)) => (Ok(command), secrets),
                Err(err) => (Err(err), None),
            }
        }
        Port::AnyPs5 => (
            anyps5_command(this, root, &toolchain, game, profile_id, cache_dir, cx).await,
            None,
        ),
    };
    let command = command.map_err(|err| {
        tracing::warn!(port = port.name(), title_id = %game.title_id, error = %err, "could not start game");
        err.localized()
    });
    let facts = jobs::port_facts(root, port, Some(&toolchain)).await;
    let (command, facts) = vk_layer::apply(this, root, &toolchain, &game.dir, command, facts, cx).await;
    let command = match command {
        Ok(command) => command,
        Err(err) => return Err(err),
    };
    Ok((
        Launcher::Command(launch::around_port(root, game, command).await, secrets),
        facts,
    ))
}

/// What a launch from the window takes from it: what the port's last report offers, and the ratio
/// of the display the game opens on.
pub(super) fn surroundings(cx: &mut AsyncApp) -> Surroundings<impl FnOnce() -> Option<&'static str>> {
    let (is_online_offered, is_handoff_listed, is_frame_gen_offered) = cx.update(|cx| {
        let caps = Caps::get(cx);
        (
            caps.is_offered(Feature::PsOnline),
            caps.is_offered(Feature::PsSecretsHandoff),
            caps.is_offered(Feature::PsFrameGen),
        )
    });
    let mut cx = cx.clone();
    Surroundings {
        online: OnlinePlay::AsSet {
            is_offered: is_online_offered,
            is_handoff_listed,
        },
        is_frame_gen_offered,
        display_ratio: move || display_aspect_ratio(&mut cx),
    }
}

fn notify(this: &WeakEntity<Ps>, text: Text, error: bool, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.notice = Some((text, error));
        cx.notify();
    });
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
    launch::anyps5_run_command(root, toolchain, game, profile_id, cache_dir).await
}

/// The port caught the game rewriting frames in flight and stopped overlapping them; the next
/// run starts that way, so its first frames are not corrupted while the guard catches it again.
async fn record_frame_overlap_off(this: &WeakEntity<Ps>, root: &Root, title_id: &str, line: String, cx: &mut AsyncApp) {
    match run::save_frame_overlap_off(root, title_id, line).await {
        Ok(()) => {
            let _ = this.update(cx, Ps::learned_changed);
        }
        Err(err) => {
            tracing::warn!(title_id, %err, "could not record that frame overlap is off for the title");
            notify(this, Text::new("ps.learned.save_error").arg("error", err), true, cx);
        }
    }
}

async fn record_failures(this: &WeakEntity<Ps>, dir: PathBuf, path: PathBuf, found: Vec<Failure>, cx: &mut AsyncApp) {
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
    let saved = run::save_failures(path, found).await;
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
        this.run_notices.insert(dir, (text, error));
        this.failures_changed(cx);
    });
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
            offline,
        }) => Text::new("ps.failures.calls.summary")
            .arg("unimplemented", unimplemented)
            .arg("stubs", stubs)
            .arg("failed", failed)
            .arg("status", status)
            .arg("offline", offline),
        Some(Summary::Exit(Some(code))) => Text::new("ps.failures.exit_code").arg("code", code),
        Some(Summary::Exit(None)) => Text::new("ps.failures.exit_signal"),
        None => Text::verbatim(&failure.message),
    }
}
