//! Playing a game: its port made ready first, a PS5 game converted first,
//! then the game run with its output scanned for failures, which go to the
//! profile's own record.

use std::path::{Path, PathBuf};

use decanter_ps::anyps5::{self, AnyPs5};
use decanter_ps::bbport::icons::IconStyle;
use decanter_ps::bbport::{self, Bbport, LauncherSettings};
use decanter_ps::failures::{self, Failure, Scanner};
use decanter_ps::library::Game;
use decanter_ps::port::Port;
use decanter_ps::toolchain::Toolchain;
use gpui::{AppContext as _, AsyncApp, Context, WeakEntity};
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
        let game = entry.game.clone();
        let prepared = self.prepare(port, cx);
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let toolchain = match prepared.await {
                Ok(toolchain) => toolchain,
                Err(err) => return notify(&this, format!("No se pudo preparar {}: {err}", port.name()), true, cx),
            };
            let command = match port {
                Port::Bbport => bbport_command(&toolchain, &game, &profile_id, cx).await,
                Port::AnyPs5 => anyps5_command(&this, &toolchain, &game, &profile_id, cx).await,
            };
            let command = match command {
                Ok(command) => command,
                Err(err) => return notify(&this, err, true, cx),
            };
            let commit = failures::commit_of(&port.checkout()).await;
            let scanner = Scanner::new(&game.dir, &game.title_id, &game.version, commit);
            let finished = jobs::run(
                &this,
                JobKey::Game(game.dir.clone()),
                "juego",
                command,
                Some(scanner),
                cx,
            )
            .await;
            record_failures(&this, &profile_id, finished.failures, cx).await;
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
        self.notice = Some(("Fusionando la actualización…".to_owned(), false));
        cx.notify();
        let task = cx.background_spawn(async move { bbport::merge_update(&base, &update, bbport::SUPPORTED_VERSION) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.notice = Some(match result {
                    Ok(merged) => (format!("Creado {}", merged.display()), false),
                    Err(err) => (format!("No se pudo fusionar: {err}"), true),
                });
                this.rescan(cx);
            });
        })
        .detach();
    }
}

fn notify(this: &WeakEntity<Ps>, text: String, error: bool, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.notice = Some((text, error));
        cx.notify();
    });
}

async fn bbport_command(
    toolchain: &Toolchain,
    game: &Game,
    profile_id: &str,
    cx: &mut AsyncApp,
) -> Result<Command, String> {
    let settings = cx.background_spawn(async { LauncherSettings::load() }).await;
    let bbport = Bbport::new(Port::Bbport.checkout());
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
    // The layout's button icons are built here when they need to be: a few
    // hundred milliseconds of decoding and encoding, off the UI thread.
    let mods = cx
        .background_spawn(async move { bbport::mods::prepare(&game_dir, &for_mods, detected) })
        .await;
    let mods = match mods {
        Ok(mods) => mods,
        Err(err) => return Err(format!("No se pudieron preparar los mods y los iconos: {err}")),
    };
    let user_dir = decanter_ps::paths::profile(profile_id).join("bbport/user");
    if let Err(err) = tokio::fs::create_dir_all(&user_dir).await {
        return Err(format!("No se pudo crear {}: {err}", user_dir.display()));
    }
    bbport
        .launch_command(toolchain, &game.dir, &user_dir, &settings, &mods)
        .await
        .map_err(|err| format!("No se pudo preparar el entorno: {err}"))
}

/// Converts first when the game has no conversion yet; the conversion is
/// shared, the run directory is the profile's.
async fn anyps5_command(
    this: &WeakEntity<Ps>,
    toolchain: &Toolchain,
    game: &Game,
    profile_id: &str,
    cx: &mut AsyncApp,
) -> Result<Command, String> {
    let anyps5 = AnyPs5::new(Port::AnyPs5.checkout());
    if !AnyPs5::is_converted(&game.title_id).await {
        let prepared = {
            let checkout = Port::AnyPs5.checkout();
            let game = game.clone();
            cx.background_spawn(async move { AnyPs5::new(checkout).prepare_output(&game) })
                .await
        };
        if let Err(err) = prepared {
            return Err(format!("No se pudo preparar la conversión: {err}"));
        }
        let command = match anyps5.convert_command(toolchain, game).await {
            Ok(command) => command,
            Err(err) => return Err(format!("No se pudo preparar el entorno: {err}")),
        };
        let finished = jobs::run(this, JobKey::Game(game.dir.clone()), "conversión", command, None, cx).await;
        if !finished.succeeded() {
            return Err("La conversión falló; mira su registro".to_owned());
        }
    }
    let run_dir = decanter_ps::paths::profile(profile_id)
        .join("anyps5")
        .join(&game.title_id);
    let laid_out = {
        let title_id = game.title_id.clone();
        let run_dir = run_dir.clone();
        cx.background_spawn(async move { anyps5::prepare_run_dir(&title_id, &run_dir) })
            .await
    };
    if let Err(err) = laid_out {
        return Err(format!("No se pudo preparar {}: {err}", run_dir.display()));
    }
    anyps5
        .run_command(toolchain, &game.title_id, &run_dir)
        .await
        .map_err(|err| format!("No se pudo preparar el entorno: {err}"))
}

async fn record_failures(this: &WeakEntity<Ps>, profile_id: &str, found: Vec<Failure>, cx: &mut AsyncApp) {
    if found.is_empty() {
        return;
    }
    let fatal = found
        .iter()
        .find(|failure| failure.kind != failures::Kind::Error)
        .map(|failure| failure.message.clone());
    let count = found.len();
    let path = decanter_ps::paths::profile(profile_id).join("failures.jsonl");
    let saved = cx
        .background_spawn(async move {
            let mut log = failures::Log::load(path);
            log.append(found)
        })
        .await;
    let (text, error) = match (saved, fatal) {
        (Err(err), _) => (format!("No se pudo guardar el registro de fallos: {err}"), true),
        (Ok(()), Some(message)) => (format!("El juego falló: {message} (ver Fallos)"), true),
        (Ok(()), None) => (format!("{count} errores registrados en Fallos"), false),
    };
    let _ = this.update(cx, |this, cx| {
        this.notice = Some((text, error));
        this.failures_changed(cx);
    });
}
