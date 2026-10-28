//! psport's branch built on this machine where decanter runs `psport.exe` directly
//! ([`decanter_ps::os::windows::winbuild`]): the newest commit of the branch, the owner's confirmation before
//! the first build of each commit, the build's progress and output, the builds in the library, which
//! one Jugar runs, and deleting them and the toolchain.

use std::collections::VecDeque;

use decanter_core::runner::{Pin, Progress, Runner};
use decanter_ps::os::windows::winbuild::{self, Built, Commit, Head};
use decanter_ps::paths::{self, Root};
use gpui::{AsyncApp, Context, Task, WeakEntity};

use super::Ps;
use super::package;
use crate::i18n::Text;
use crate::runners;

/// The last lines of a build's output the card shows; the whole of it is in its log file.
const SHOWN_LINES: usize = 12;

#[derive(Default)]
pub struct BranchState {
    /// The branch's newest commit, from the last check.
    head: Option<Result<Head, Text>>,
    checking: bool,
    /// The commit whose confirmation the card is asking for.
    confirming: Option<Head>,
    /// The build running, which dropping stops: its tools end with their Job Object.
    building: Option<(Head, Task<()>)>,
    stage: Option<Progress>,
    lines: VecDeque<String>,
    error: Option<Text>,
    built: Vec<Built>,
    toolchain_installed: bool,
    /// A delete under way, which a build or another delete waits out.
    deleting: bool,
}

impl BranchState {
    pub const fn head(&self) -> Option<&Result<Head, Text>> {
        self.head.as_ref()
    }

    pub const fn is_checking(&self) -> bool {
        self.checking
    }

    pub const fn confirming(&self) -> Option<&Head> {
        self.confirming.as_ref()
    }

    pub fn building(&self) -> Option<&Head> {
        self.building.as_ref().map(|(head, _)| head)
    }

    pub const fn stage(&self) -> Option<&Progress> {
        self.stage.as_ref()
    }

    pub const fn lines(&self) -> &VecDeque<String> {
        &self.lines
    }

    pub const fn error(&self) -> Option<&Text> {
        self.error.as_ref()
    }

    pub fn built(&self) -> &[Built] {
        &self.built
    }

    pub const fn is_toolchain_installed(&self) -> bool {
        self.toolchain_installed
    }

    pub const fn is_deleting(&self) -> bool {
        self.deleting
    }
}

impl Ps {
    pub const fn branch(&self) -> &BranchState {
        &self.branch
    }

    /// The builds and the toolchain in the library, read again without asking the network.
    pub fn branch_reload(&mut self, cx: &mut Context<Self>) {
        let root = paths::active();
        cx.spawn(async move |this, cx| reload(&this, &root, cx).await).detach();
    }

    /// Asks the remote for the branch's newest commit.
    pub fn branch_check(&mut self, cx: &mut Context<Self>) {
        // A delete under way may be taking the MinGit the check installs.
        if self.branch.checking || self.branch.deleting {
            return;
        }
        self.branch.checking = true;
        cx.notify();
        let root = paths::active();
        cx.spawn(async move |this, cx| {
            let head = match winbuild::git::program(&root, &mut |_| {}).await {
                Ok(program) => winbuild::git::head(&program, &winbuild::work(&root)).await,
                Err(err) => Err(err),
            }
            .map_err(|err| {
                tracing::warn!(%err, "could not ask psport's remote for its branch");
                runners::build_error(&err)
            });
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.branch;
                state.checking = false;
                state.head = Some(head);
                cx.notify();
            });
        })
        .detach();
    }

    /// The card's Compilar: builds a commit confirmed before, or asks for the confirmation first.
    pub fn branch_build(&mut self, head: &Head, cx: &mut Context<Self>) {
        let (root, head) = (paths::active(), head.clone());
        cx.spawn(async move |this, cx| {
            if winbuild::is_confirmed(&root, &head.commit).await {
                let _ = this.update(cx, |this, cx| this.start_build(&root, head, cx));
            } else {
                let _ = this.update(cx, |this, cx| {
                    this.branch.confirming = Some(head);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// The owner's yes to the commit the card showed, recorded before the build starts.
    pub fn branch_confirm(&mut self, cx: &mut Context<Self>) {
        let Some(head) = self.branch.confirming.take() else {
            return;
        };
        cx.notify();
        let root = paths::active();
        cx.spawn(async move |this, cx| {
            if let Err(err) = winbuild::confirm(&root, &head).await {
                tracing::warn!(%err, "could not record the build's confirmation");
                return set_error(&this, Some(Text::new("ps.branch.errors.confirm").arg("error", err)), cx);
            }
            let _ = this.update(cx, |this, cx| this.start_build(&root, head, cx));
        })
        .detach();
    }

    pub fn branch_cancel_confirm(&mut self, cx: &mut Context<Self>) {
        self.branch.confirming = None;
        cx.notify();
    }

    /// Stops the build: its task goes, and with it the tools' Job Object.
    pub fn branch_stop(&mut self, cx: &mut Context<Self>) {
        if let Some((head, task)) = self.branch.building.take() {
            tracing::info!(commit = %head.commit, "Windows build stopped by the user");
            drop(task);
            self.branch.stage = None;
            self.branch.error = Some(Text::new("ps.branch.stopped"));
            cx.notify();
            self.branch_reload(cx);
        }
    }

    fn start_build(&mut self, root: &Root, head: Head, cx: &mut Context<Self>) {
        if self.branch.building.is_some() || self.branch.deleting {
            self.branch.error = Some(Text::new("ps.branch.errors.busy"));
            cx.notify();
            return;
        }
        let root = root.clone();
        let task = cx.spawn({
            let head = head.clone();
            async move |this, cx| run_build(&this, &root, &head, cx).await
        });
        let state = &mut self.branch;
        state.building = Some((head, task));
        state.stage = None;
        state.lines.clear();
        state.error = None;
        cx.notify();
    }

    /// Jugar runs `commit`'s build from now on; the release updates leave it in place.
    pub fn branch_use(&mut self, commit: &Commit, cx: &mut Context<Self>) {
        let (root, commit) = (paths::active(), commit.clone());
        cx.spawn(async move |this, cx| {
            if let Err(err) = package::select(&this, &root, commit.as_str(), true, cx).await {
                set_error(&this, Some(err), cx);
            }
        })
        .detach();
    }

    /// Never the build in use, nor any while a choice is being saved, a game runs or a build runs.
    pub fn branch_delete(&mut self, commit: &Commit, cx: &mut Context<Self>) {
        if self.package.selected_build() == Some(commit) || !self.may_delete(cx) {
            self.branch.error = Some(Text::new("ps.branch.errors.delete_in_use"));
            cx.notify();
            return;
        }
        self.branch.deleting = true;
        cx.notify();
        let (root, commit) = (paths::active(), commit.clone());
        cx.spawn(async move |this, cx| {
            let result = if package::settings_now(&root, cx).await.selected_build.as_ref() == Some(&commit) {
                Err(Text::new("ps.branch.errors.delete_in_use"))
            } else {
                runners::remove(&root, Runner::Psport, &Pin::Commit(commit.clone()))
                    .await
                    .map_err(|err| {
                        tracing::warn!(%commit, %err, "could not remove a Windows build");
                        err.text()
                    })
            };
            if result.is_ok() {
                tracing::info!(%commit, "Windows build removed");
            }
            finish_delete(&this, &root, result.err(), cx).await;
        })
        .detach();
    }

    /// The downloaded toolchain, never while a game or a build runs; the next build downloads it again.
    pub fn branch_delete_toolchain(&mut self, cx: &mut Context<Self>) {
        self.delete_toolchain(false, cx);
    }

    /// Delete the toolchain a check found changed, then check again so it is fetched afresh.
    pub fn branch_redownload_toolchain(&mut self, cx: &mut Context<Self>) {
        self.delete_toolchain(true, cx);
    }

    fn delete_toolchain(&mut self, then_check: bool, cx: &mut Context<Self>) {
        if !self.may_delete(cx) {
            self.branch.error = Some(Text::new("ps.branch.errors.delete_toolchain_busy"));
            cx.notify();
            return;
        }
        self.branch.deleting = true;
        cx.notify();
        tracing::info!("removing the Windows toolchain");
        let root = paths::active();
        cx.spawn(async move |this, cx| {
            let result = winbuild::toolchain::remove(&root).await.map_err(|err| {
                tracing::warn!(%err, "could not remove the Windows toolchain");
                Text::new("ps.branch.errors.delete").arg("error", err)
            });
            if result.is_ok() {
                tracing::info!("Windows toolchain removed");
            }
            let deleted = result.is_ok();
            finish_delete(&this, &root, result.err(), cx).await;
            if then_check && deleted {
                let _ = this.update(cx, Ps::branch_check);
            }
        })
        .detach();
    }

    fn may_delete(&self, cx: &Context<Self>) -> bool {
        let playing = self.jobs.values().any(|job| job.read(cx).is_running());
        !playing
            && self.branch.building.is_none()
            && !self.branch.checking
            && !self.branch.deleting
            && !self.package.is_selecting()
    }
}

async fn finish_delete(this: &WeakEntity<Ps>, root: &Root, error: Option<Text>, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.branch.deleting = false;
        this.branch.error = error;
        cx.notify();
    });
    reload(this, root, cx).await;
}

fn set_error(this: &WeakEntity<Ps>, error: Option<Text>, cx: &mut AsyncApp) {
    let _ = this.update(cx, |this, cx| {
        this.branch.error = error;
        cx.notify();
    });
}

async fn reload(this: &WeakEntity<Ps>, root: &Root, cx: &mut AsyncApp) {
    let built = winbuild::built(root).await;
    let toolchain_installed = winbuild::toolchain::is_installed(root).await;
    let _ = this.update(cx, |this, cx| {
        this.branch.built = built;
        this.branch.toolchain_installed = toolchain_installed;
        cx.notify();
    });
    package::reload(this, root, cx).await;
}

async fn run_build(this: &WeakEntity<Ps>, root: &Root, head: &Head, cx: &mut AsyncApp) {
    let (for_stage, mut stage_cx) = (this.clone(), cx.clone());
    let mut on_progress = move |progress: Progress| {
        let _ = for_stage.update(&mut stage_cx, |this, cx| {
            this.branch.stage = Some(progress);
            cx.notify();
        });
    };
    let (for_lines, mut lines_cx) = (this.clone(), cx.clone());
    let mut on_line = move |line: String| {
        let _ = for_lines.update(&mut lines_cx, |this, cx| {
            let lines = &mut this.branch.lines;
            if lines.len() == SHOWN_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
            cx.notify();
        });
    };
    let pin = Pin::Commit(head.commit.clone());
    let result = runners::prepare(root, Runner::Psport, &pin, &mut on_progress, &mut on_line).await;
    let error = result.as_ref().err().map(runners::Error::text);
    // Reloaded before `building` is cleared: dropping that task is dropping this one, so nothing after would run.
    reload(this, root, cx).await;
    let _ = this.update(cx, |this, cx| {
        let state = &mut this.branch;
        state.building = None;
        state.stage = None;
        state.error = error;
        cx.notify();
    });
}
