//! The Raven section's state (decision #1069): Raven's builds in the active library, each built
//! only when the player confirms it, at the pinned commit or at the newest commit of Raven's branch
//! once a check asked for it (#1238), which one runs, and the Windows programs of the active profile
//! that run with it.
//!
//! Created the first time the section opens or a file is dropped on it, and kept after that, so a
//! build outlives switching to another section. Cancelling a build is dropping its task: the
//! build's tools end with their process group ([`decanter_runtime::runner`]).

use std::path::PathBuf;

use decanter_core::InstalledGame;
use decanter_core::runner::{Commit, Pin, Progress, Runner, Stage};
use decanter_ps::paths;
use decanter_runtime::runner::{self, RavenHead};
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
    built: Vec<Built>,
    /// The commit Raven runs at, which may not be built.
    in_use: Option<Commit>,
    /// The branch's newest commit, from the last check.
    head: Option<Result<RavenHead, Text>>,
    checking: bool,
    /// The commit whose build the section asks the confirmation for.
    confirming: Option<Commit>,
    /// The confirmation the section asks for before removing Raven.
    confirming_remove: bool,
    /// The build whose delete waits for the confirmation.
    confirming_delete: Option<Commit>,
    /// The build running, which dropping stops.
    building: Option<(Commit, Task<()>)>,
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
                built: Vec::new(),
                in_use: None,
                head: None,
                checking: false,
                confirming: None,
                confirming_remove: false,
                confirming_delete: None,
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

    pub fn built(&self) -> &[Built] {
        &self.built
    }

    pub const fn in_use(&self) -> Option<&Commit> {
        self.in_use.as_ref()
    }

    /// The build Raven runs, when it is there.
    pub fn in_use_built(&self) -> Option<&Built> {
        self.built
            .iter()
            .find(|built| Some(&built.commit) == self.in_use.as_ref())
    }

    pub fn is_built(&self, commit: &Commit) -> bool {
        self.built.iter().any(|built| &built.commit == commit)
    }

    pub const fn head(&self) -> Option<&Result<RavenHead, Text>> {
        self.head.as_ref()
    }

    pub const fn is_checking(&self) -> bool {
        self.checking
    }

    pub const fn confirming(&self) -> Option<&Commit> {
        self.confirming.as_ref()
    }

    pub const fn is_confirming_remove(&self) -> bool {
        self.confirming_remove
    }

    pub const fn confirming_delete(&self) -> Option<&Commit> {
        self.confirming_delete.as_ref()
    }

    pub fn building(&self) -> Option<&Commit> {
        self.building.as_ref().map(|(commit, _)| commit)
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

    /// The build log, one for every commit, where there is a library to keep it in.
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

    /// Asks Raven's repository for its branch's newest commit; only when the player presses the
    /// section's check, never on its own.
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if self.checking {
            return;
        }
        self.checking = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let head = runner::raven_head().await.map_err(|err| {
                tracing::warn!(error = %format_args!("{err:#}"), "could not ask Raven's repository for its branch");
                Text::new("raven.update.check_error").arg("error", format!("{err:#}"))
            });
            if let Ok(head) = &head {
                tracing::info!(branch = %head.branch, commit = %head.commit, "Raven's branch checked");
            }
            let _ = this.update(cx, |this, cx| {
                this.checking = false;
                this.head = Some(head);
                cx.notify();
            });
        })
        .detach();
    }

    /// The section's Compilar for `commit`: the confirmation comes first, every time, since it
    /// says what the build downloads and takes.
    pub fn ask_build(&mut self, commit: &Commit, cx: &mut Context<Self>) {
        if self.building.is_none() && !self.removing {
            self.confirming = Some(commit.clone());
            self.confirming_remove = false;
            self.confirming_delete = None;
            self.error = None;
            cx.notify();
        }
    }

    pub fn cancel_confirm(&mut self, cx: &mut Context<Self>) {
        self.confirming = None;
        cx.notify();
    }

    pub fn ask_remove(&mut self, cx: &mut Context<Self>) {
        if self.building.is_none() && !self.removing {
            self.confirming_remove = true;
            self.confirming = None;
            self.confirming_delete = None;
            self.error = None;
            cx.notify();
        }
    }

    pub fn cancel_remove(&mut self, cx: &mut Context<Self>) {
        self.confirming_remove = false;
        cx.notify();
    }

    /// The player's yes, recorded before the build starts.
    pub fn confirm_build(&mut self, cx: &mut Context<Self>) {
        let Some(commit) = self.confirming.take() else {
            return;
        };
        if self.building.is_some() || self.removing {
            cx.notify();
            return;
        }
        self.progress = None;
        self.error = None;
        self.log_tail.clear();
        let task = cx.spawn({
            let commit = commit.clone();
            async move |this, cx| build(&this, &commit, cx).await
        });
        self.building = Some((commit, task));
        cx.notify();
    }

    /// Stops the build: its task goes, and with it the build's tools and its folder.
    pub fn stop_build(&mut self, cx: &mut Context<Self>) {
        if let Some((_, task)) = self.building.take() {
            tracing::info!("Raven build stopped by the player");
            drop(task);
            self.progress = None;
            self.error = Some(Text::new("raven.build.stopped"));
            cx.notify();
            self.reload(cx);
        }
    }

    /// Removes every build of Raven and, with the last, its toolchain; never while a program runs
    /// or a build runs.
    pub fn remove_build(&mut self, cx: &mut Context<Self>) {
        self.confirming_remove = false;
        // The build in use goes last, so a removal cut short leaves Raven runnable.
        let mut commits: Vec<Commit> = self.built.iter().map(|built| built.commit.clone()).collect();
        commits.sort_by_key(|commit| Some(commit) == self.in_use.as_ref());
        self.remove(commits, cx);
    }

    /// Makes `commit`'s build the one Raven runs from the next launch on.
    pub fn use_build(&mut self, commit: &Commit, cx: &mut Context<Self>) {
        if self.building.is_some() || self.removing {
            return;
        }
        let commit = commit.clone();
        cx.spawn(async move |this, cx| {
            let chosen = runner::raven_use(&commit).await;
            if let Err(err) = &chosen {
                tracing::warn!(%commit, %err, "could not choose the Raven build to run");
            }
            let _ = this.update(cx, |this, cx| {
                this.error = chosen.err().map(|err| Text::new("raven.errors.use").arg("error", err));
                cx.notify();
            });
            reload(&this, cx).await;
        })
        .detach();
    }

    pub fn ask_delete(&mut self, commit: &Commit, cx: &mut Context<Self>) {
        if self.building.is_none() && !self.removing {
            self.confirming_delete = Some(commit.clone());
            self.confirming = None;
            self.confirming_remove = false;
            self.error = None;
            cx.notify();
        }
    }

    pub fn cancel_delete(&mut self, cx: &mut Context<Self>) {
        self.confirming_delete = None;
        cx.notify();
    }

    /// Deletes a build that is not the one in use.
    pub fn delete_build(&mut self, cx: &mut Context<Self>) {
        let Some(commit) = self.confirming_delete.take() else {
            return;
        };
        if self.in_use.as_ref() == Some(&commit) {
            cx.notify();
            return;
        }
        self.remove(vec![commit], cx);
    }

    fn remove(&mut self, commits: Vec<Commit>, cx: &mut Context<Self>) {
        let running = &AppState::get(cx).background.read(cx).running;
        if self.programs.iter().any(|program| running.contains(&program.game_id)) {
            self.error = Some(Text::new("raven.errors.remove_running"));
            cx.notify();
            return;
        }
        if self.building.is_some() || self.removing {
            return;
        }
        self.removing = true;
        self.error = None;
        cx.notify();
        let root = paths::active();
        cx.spawn(async move |this, cx| {
            let mut failed = None;
            for commit in commits {
                if let Err(err) = runners::remove(&root, Runner::Raven, &Pin::Commit(commit.clone())).await {
                    tracing::warn!(%commit, %err, "could not remove a Raven build");
                    failed = Some(err.text());
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.removing = false;
                this.log_tail.clear();
                this.error = failed;
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
    let (built, in_use) = cx
        .background_spawn(async move {
            let status = runners::status(&root, Runner::Raven).await;
            let built = status
                .installed()
                .iter()
                .filter_map(|installed| match &installed.pin {
                    Pin::Commit(commit) => Some(commit.clone()),
                    Pin::Release(_) => None,
                })
                .map(|commit| {
                    // The commit's folder: the checkout and its build tree, which Raven runs from.
                    let size = runner::raven_dir()
                        .and_then(|folder| decanter_core::directory_size(&folder.join(commit.as_str())).ok());
                    Built { commit, size }
                })
                .collect();
            (built, runner::raven_in_use())
        })
        .await;
    let _ = this.update(cx, |this, cx| {
        this.built = built;
        this.in_use = in_use;
        cx.notify();
    });
}

/// What was just built is what runs, and the build it replaces goes ([`runner::raven_prune`]).
async fn build(this: &WeakEntity<Raven>, commit: &Commit, cx: &mut AsyncApp) {
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
    let pin = Pin::Commit(commit.clone());
    let built = match runners::confirm(&root, Runner::Raven, &pin).await {
        Ok(()) => runners::prepare(&root, Runner::Raven, &pin, &mut report, &mut |_| {}).await,
        Err(err) => Err(err),
    };
    let (error, log_tail) = match built {
        Ok(_) => (adopt(commit).await, Vec::new()),
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

async fn adopt(commit: &Commit) -> Option<Text> {
    if let Err(err) = runner::raven_use(commit).await {
        tracing::warn!(%commit, %err, "Raven built, but could not be chosen to run");
        return Some(Text::new("raven.errors.use").arg("error", err));
    }
    runner::raven_prune().await;
    None
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
