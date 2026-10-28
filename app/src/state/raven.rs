//! The Raven section's state (decision #1069): Raven's build in the active library, built only
//! when the player confirms it, and the Windows programs of the active profile that run with it.
//!
//! Created the first time the section opens or a file is dropped on it, and kept after that, so a
//! build outlives switching to another section. Cancelling a build is dropping its task: the
//! build's tools end with their process group ([`decanter_runtime::runner`]).

use std::path::PathBuf;

use decanter_core::InstalledGame;
use decanter_core::runner::{Commit, Pin, Progress, Runner, Stage};
use decanter_ps::paths;
use decanter_runtime::runner;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{App, AppContext as _, AsyncApp, Context, Entity, Global, Task, WeakEntity};

use super::AppState;
use crate::i18n::Text;
use crate::runners;

/// The end of a failed build's log the section shows; the whole of it is in the file.
const SHOWN_LINES: usize = 12;

/// A finished build: its commit and what its folder takes on disk, once measured.
pub struct Built {
    pub commit: Commit,
    pub size: Option<u64>,
}

pub struct Raven {
    profile_id: Option<String>,
    programs: Vec<InstalledGame>,
    built: Option<Built>,
    /// The confirmation the section asks for before a build.
    confirming: bool,
    /// The build running, which dropping stops.
    building: Option<Task<()>>,
    progress: Option<Progress>,
    removing: bool,
    error: Option<Text>,
    /// The end of the build log after a failed build.
    log_tail: Vec<String>,
    /// What the latest add, drop or removal did, a line per file, and whether any failed.
    notice: Option<(Vec<Text>, bool)>,
}

struct RavenGlobal(Entity<Raven>);

impl Global for RavenGlobal {}

impl Raven {
    pub fn get(cx: &mut App) -> Entity<Self> {
        if let Some(global) = cx.try_global::<RavenGlobal>() {
            return global.0.clone();
        }
        let session = AppState::get(cx).session.clone();
        let raven = cx.new(|cx| {
            cx.observe(&session, |this: &mut Self, _, cx| this.sync_profile(cx))
                .detach();
            let mut this = Self {
                profile_id: None,
                programs: Vec::new(),
                built: None,
                confirming: false,
                building: None,
                progress: None,
                removing: false,
                error: None,
                log_tail: Vec::new(),
                notice: None,
            };
            this.sync_profile(cx);
            this.reload(cx);
            this
        });
        cx.set_global(RavenGlobal(raven.clone()));
        raven
    }

    pub fn programs(&self) -> &[InstalledGame] {
        &self.programs
    }

    pub const fn profile_id(&self) -> Option<&String> {
        self.profile_id.as_ref()
    }

    pub const fn built(&self) -> Option<&Built> {
        self.built.as_ref()
    }

    pub const fn is_confirming(&self) -> bool {
        self.confirming
    }

    pub const fn is_building(&self) -> bool {
        self.building.is_some()
    }

    pub const fn progress(&self) -> Option<&Progress> {
        self.progress.as_ref()
    }

    pub const fn is_removing(&self) -> bool {
        self.removing
    }

    pub const fn error(&self) -> Option<&Text> {
        self.error.as_ref()
    }

    pub fn log_tail(&self) -> &[String] {
        &self.log_tail
    }

    pub const fn notice(&self) -> Option<&(Vec<Text>, bool)> {
        self.notice.as_ref()
    }

    /// The build log of the pinned commit, where there is a library to keep it in.
    pub fn log_path() -> Option<PathBuf> {
        runner::raven_pin().and_then(|pin| runners::log_path(&paths::active(), Runner::Raven, &pin))
    }

    fn sync_profile(&mut self, cx: &mut Context<Self>) {
        let active = AppState::get(cx).session.read(cx).active_profile.clone();
        if active != self.profile_id {
            self.profile_id = active;
            self.reload_programs(cx);
        }
    }

    fn reload_programs(&mut self, cx: &mut Context<Self>) {
        let Some(profile_id) = self.profile_id.clone() else {
            self.programs.clear();
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let programs = cx
                .background_spawn(async move { crate::raven::list(&profile_id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.programs = programs;
                cx.notify();
            });
        })
        .detach();
    }

    /// What is built, read from disk, with the build's size measured off the UI thread.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| reload(&this, cx).await).detach();
    }

    /// The section's Compilar: the confirmation comes first, every time, since it says what the
    /// build downloads and takes.
    pub fn ask_build(&mut self, cx: &mut Context<Self>) {
        if self.building.is_none() && !self.removing {
            self.confirming = true;
            self.error = None;
            cx.notify();
        }
    }

    pub fn cancel_confirm(&mut self, cx: &mut Context<Self>) {
        self.confirming = false;
        cx.notify();
    }

    /// The player's yes, recorded before the build starts.
    pub fn confirm_build(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.confirming) || self.building.is_some() {
            return;
        }
        let Some(pin) = runner::raven_pin() else {
            self.error = Some(Text::new("raven.errors.not_pinned"));
            cx.notify();
            return;
        };
        self.progress = None;
        self.error = None;
        self.log_tail.clear();
        self.building = Some(cx.spawn(async move |this, cx| build(&this, &pin, cx).await));
        cx.notify();
    }

    /// Stops the build: its task goes, and with it the build's tools and its folder.
    pub fn stop_build(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.building.take() {
            tracing::info!("Raven build stopped by the player");
            drop(task);
            self.progress = None;
            self.error = Some(Text::new("raven.build.stopped"));
            cx.notify();
            self.reload(cx);
        }
    }

    /// Removes Raven's build and toolchain, never while a program runs or a build runs.
    pub fn remove_build(&mut self, cx: &mut Context<Self>) {
        let running = &AppState::get(cx).background.read(cx).running;
        if self.programs.iter().any(|program| running.contains(&program.game_id)) {
            self.error = Some(Text::new("raven.errors.remove_running"));
            cx.notify();
            return;
        }
        let Some(pin) = runner::raven_pin() else {
            return;
        };
        if self.building.is_some() || self.removing {
            return;
        }
        self.removing = true;
        self.error = None;
        cx.notify();
        let root = paths::active();
        cx.spawn(async move |this, cx| {
            let removed = runners::remove(&root, Runner::Raven, &pin).await;
            let _ = this.update(cx, |this, cx| {
                this.removing = false;
                this.log_tail.clear();
                if let Err(err) = removed {
                    tracing::warn!(%err, "could not remove Raven");
                    this.error = Some(err.text());
                }
                cx.notify();
            });
            reload(&this, cx).await;
        })
        .detach();
    }

    /// Adds each Windows program among `paths` to the active profile, and says what came of it.
    pub fn add(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(profile_id) = self.profile_id.clone() else {
            self.notice = Some((vec![Text::new("external.drop.no_profile")], true));
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    paths
                        .iter()
                        .map(|path| {
                            let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                            (file, crate::raven::add(&profile_id, path))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.notice = notice(outcome);
                this.reload_programs(cx);
            });
        })
        .detach();
    }

    /// Forgets the program and its prefix; never while it runs.
    pub fn remove_program(&mut self, id: &str, cx: &mut Context<Self>) {
        let (Some(profile_id), id) = (self.profile_id.clone(), id.to_owned()) else {
            return;
        };
        if AppState::get(cx).background.read(cx).running.contains(&id) {
            self.notice = Some((vec![Text::new("raven.errors.remove_running")], true));
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let removed = cx
                .background_spawn(async move { crate::raven::remove(&profile_id, &id) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.notice = removed.err().map(|err| (vec![err], true));
                this.reload_programs(cx);
            });
        })
        .detach();
    }
}

/// A line per file: what it was added as, or why it was refused.
fn notice(outcome: Vec<(String, Result<InstalledGame, Text>)>) -> Option<(Vec<Text>, bool)> {
    let is_error = outcome.iter().any(|(_, result)| result.is_err());
    let lines: Vec<Text> = outcome
        .into_iter()
        .map(|(file, result)| match result {
            Ok(program) => Text::new("raven.programs.added").arg("name", program.name),
            Err(reason) => Text::new("external.drop.refused.line")
                .arg("file", file)
                .text("reason", reason),
        })
        .collect();
    (!lines.is_empty()).then_some((lines, is_error))
}

async fn reload(this: &WeakEntity<Raven>, cx: &mut AsyncApp) {
    let root = paths::active();
    let built = cx
        .background_spawn(async move {
            let status = runners::status(&root, Runner::Raven).await;
            let installed = status.installed().iter().find_map(|installed| match &installed.pin {
                Pin::Commit(commit) => Some(commit.clone()),
                Pin::Release(_) => None,
            });
            installed.map(|commit| {
                // The commit's folder: the checkout and its build tree, which Raven runs from.
                let size = runner::raven_dir()
                    .and_then(|folder| decanter_core::directory_size(&folder.join(commit.as_str())).ok());
                Built { commit, size }
            })
        })
        .await;
    let _ = this.update(cx, |this, cx| {
        this.built = built;
        cx.notify();
    });
}

async fn build(this: &WeakEntity<Raven>, pin: &Pin, cx: &mut AsyncApp) {
    let (sender, mut receiver) = mpsc::unbounded::<Progress>();
    let shown = this.clone();
    let _shown = cx.spawn(async move |cx| {
        while let Some(progress) = receiver.next().await {
            let _ = shown.update(cx, |this, cx| {
                this.progress = Some(progress);
                cx.notify();
            });
        }
    });
    // A step per Ninja step or mebibyte, since a download reports every chunk.
    let mut last = None;
    let mut report = move |progress: Progress| {
        let step = (
            core::mem::discriminant(&progress.stage),
            match progress.stage {
                Stage::Downloading => progress.received >> 20,
                Stage::Unpacking
                | Stage::Verifying
                | Stage::Sources
                | Stage::Configuring
                | Stage::Compiling
                | Stage::Assembling => progress.received,
            },
        );
        if last != Some(step) {
            last = Some(step);
            let _ = sender.unbounded_send(progress);
        }
    };
    let root = paths::active();
    let built = match runners::confirm(&root, Runner::Raven, pin).await {
        Ok(()) => runners::prepare(&root, Runner::Raven, pin, &mut report, &mut |_| {}).await,
        Err(err) => Err(err),
    };
    let (error, log_tail) = match built {
        Ok(_) => (None, Vec::new()),
        Err(err) => {
            tracing::warn!(error = %err, "Raven build failed");
            (Some(err.text()), log_tail().await)
        }
    };
    // Reloaded before `building` is cleared: dropping that task is dropping this one.
    reload(this, cx).await;
    let _ = this.update(cx, |this, cx| {
        this.building = None;
        this.progress = None;
        this.error = error;
        this.log_tail = log_tail;
        cx.notify();
    });
}

async fn log_tail() -> Vec<String> {
    let Some(path) = Raven::log_path() else {
        return Vec::new();
    };
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return Vec::new();
    };
    let mut tail: Vec<String> = text.lines().rev().take(SHOWN_LINES).map(str::to_owned).collect();
    tail.reverse();
    tail
}
