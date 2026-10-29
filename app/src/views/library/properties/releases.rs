//! Betas (which branch) and Versiones (which build within it): both a
//! remote list with ACTUAL on the installed entry and a button that
//! reinstalls the game from another one.

use std::rc::Rc;

use decanter_core::InstalledGame;
use decanter_gog::{Branch, Build};
use gpui::{Context, Window, div, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, not_installed, pane};
use crate::i18n::Text;
use crate::state::{AppState, DownloadKey, DownloadKind};
use crate::theme;
use crate::views::library::install::{self, DownloadJob, Ended};
use crate::widgets::{ButtonKind, button};

/// `use_resource`'s three states: `None` while the first load runs, then
/// the outcome — which a reload keeps showing until the new one lands.
type Remote<T> = Option<Result<Vec<T>, String>>;

pub(super) struct BetasTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    branches: Remote<Branch>,
    /// The branch being switched to, `Some(None)` for mainline.
    switching: Option<Option<String>>,
    error: Option<Text>,
}

impl BetasTab {
    pub(super) fn new(target: Rc<Target>, cx: &Context<Self>) -> Self {
        let installed = crate::core::installed_game(&target.account_id, &target.product_id);
        let this = Self {
            target,
            installed,
            branches: None,
            switching: None,
            error: None,
        };
        if this.installed.is_some() {
            this.load(cx);
        }
        this
    }

    fn load(&self, cx: &Context<Self>) {
        let target = Rc::clone(&self.target);
        cx.spawn(async move |this, cx| {
            let outcome = decanter_gog::list_branches(&target.account_id, &target.product_id)
                .await
                .map_err(|err| err.to_string());
            let _ = this.update(cx, |this, cx| {
                this.branches = Some(outcome);
                cx.notify();
            });
        })
        .detach();
    }

    fn switch(&mut self, branch_name: Option<String>, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        self.switching = Some(branch_name.clone());
        self.error = None;
        tracing::info!(
            account_id = target.account_id,
            product_id = target.product_id,
            branch = branch_name,
            "branch switch started"
        );
        cx.notify();
        let state = AppState::get(cx).clone();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let product_id = target.product_id.as_str();
            let Ok(core_library) = crate::core::library_for_account(account_id) else {
                tracing::error!(
                    account_id,
                    product_id,
                    "could not resolve library, branch switch aborted"
                );
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(Text::new("library.install.no_library"));
                    this.switching = None;
                    cx.notify();
                });
                return;
            };
            let (on_progress, progress) = install::progress_channel();
            let switch = decanter_gog::install(
                &core_library,
                account_id,
                product_id,
                None,
                &target.title,
                branch_name.as_deref(),
                None,
                on_progress,
            );
            let job = DownloadJob {
                key: DownloadKey::new(account_id, product_id),
                title: target.title.clone(),
                kind: DownloadKind::BranchSwitch,
            };
            let outcome = install::track(&state, job, progress, switch, cx).await;
            let _ = this.update(cx, |this, cx| {
                this.switching = None;
                match outcome {
                    Ended::Done(()) => {
                        tracing::info!(account_id, product_id, branch = branch_name, "branch switch finished");
                        this.load(cx);
                    }
                    Ended::Cancelled | Ended::Paused { .. } => {}
                    Ended::Failed(error) => this.error = Some(error),
                }
                this.installed = crate::core::installed_game(account_id, product_id);
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for BetasTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(installed) = &self.installed else {
            return not_installed();
        };
        let list = match &self.branches {
            None => return pane().child(theme::muted(t!("common.loading"))),
            Some(Err(err)) => return pane().child(theme::error(err.clone())),
            Some(Ok(list)) => list,
        };

        let count = list.len();
        let card = theme::card().children(list.iter().enumerate().map(|(index, branch)| {
            let label = branch.name.clone().unwrap_or_else(|| "mainline".to_string());
            let is_current = branch.name == installed.branch;
            let busy = self.switching.as_ref() == Some(&branch.name);
            let row = theme::list_row(("branch", index), index + 1 == count)
                .child(div().flex_1().child(label))
                .child(theme::muted(format!("build {}", branch.build_id)));
            if is_current {
                row.child(theme::badge(t!("library.properties.releases.current"), false))
            } else {
                let disabled = self.switching.is_some();
                let branch_name = branch.name.clone();
                row.child(
                    button(
                        ("switch-branch", index),
                        ButtonKind::Secondary,
                        if busy {
                            t!("library.properties.betas.switching")
                        } else {
                            t!("library.properties.betas.switch")
                        },
                        disabled,
                    )
                    .when(!disabled, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.switch(branch_name.clone(), cx)))
                    }),
                )
            }
        }));

        pane()
            .child(card)
            .child(theme::muted(t!("library.properties.betas.note")))
            .children(error_line(self.error.as_ref()))
    }
}

/// Every build GOG has published for the branch this game is on, newest
/// first — a manual rollback to a specific older build, distinct from
/// Betas (which picks *which branch*, always at that branch's newest).
pub(super) struct VersionsTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    /// The branch the list was first fetched for; a reload asks again for
    /// the same one, the way the webview resource's captured branch did.
    branch: Option<String>,
    builds: Remote<Build>,
    installing: Option<String>,
    error: Option<Text>,
}

impl VersionsTab {
    pub(super) fn new(target: Rc<Target>, cx: &Context<Self>) -> Self {
        let installed = crate::core::installed_game(&target.account_id, &target.product_id);
        let this = Self {
            target,
            branch: installed.as_ref().and_then(|installed| installed.branch.clone()),
            installed,
            builds: None,
            installing: None,
            error: None,
        };
        if this.installed.is_some() {
            this.load(cx);
        }
        this
    }

    fn load(&self, cx: &Context<Self>) {
        let target = Rc::clone(&self.target);
        let branch = self.branch.clone();
        cx.spawn(async move |this, cx| {
            let outcome = decanter_gog::list_builds(&target.account_id, &target.product_id, branch.as_deref())
                .await
                .map_err(|err| err.to_string());
            let _ = this.update(cx, |this, cx| {
                this.builds = Some(outcome);
                cx.notify();
            });
        })
        .detach();
    }

    fn rollback(&mut self, target_build_id: String, cx: &mut Context<Self>) {
        let Some(installed) = &self.installed else {
            return;
        };
        let branch = installed.branch.clone();
        let target = Rc::clone(&self.target);
        self.installing = Some(target_build_id.clone());
        self.error = None;
        tracing::info!(
            account_id = target.account_id,
            product_id = target.product_id,
            target_build_id,
            "build rollback started"
        );
        cx.notify();
        let state = AppState::get(cx).clone();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let product_id = target.product_id.as_str();
            let Ok(core_library) = crate::core::library_for_account(account_id) else {
                tracing::error!(account_id, product_id, "could not resolve library, rollback aborted");
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(Text::new("library.install.no_library"));
                    this.installing = None;
                    cx.notify();
                });
                return;
            };
            let (on_progress, progress) = install::progress_channel();
            let rollback = decanter_gog::install(
                &core_library,
                account_id,
                product_id,
                None,
                &target.title,
                branch.as_deref(),
                Some(&target_build_id),
                on_progress,
            );
            let job = DownloadJob {
                key: DownloadKey::new(account_id, product_id),
                title: target.title.clone(),
                kind: DownloadKind::Rollback,
            };
            let outcome = install::track(&state, job, progress, rollback, cx).await;
            let _ = this.update(cx, |this, cx| {
                this.installing = None;
                match outcome {
                    Ended::Done(()) => {
                        tracing::info!(account_id, product_id, target_build_id, "build rollback finished");
                        this.load(cx);
                    }
                    Ended::Cancelled | Ended::Paused { .. } => {}
                    Ended::Failed(error) => this.error = Some(error),
                }
                this.installed = crate::core::installed_game(account_id, product_id);
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for VersionsTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(installed) = &self.installed else {
            return not_installed();
        };
        let list = match &self.builds {
            None => return pane().child(theme::muted(t!("common.loading"))),
            Some(Err(err)) => return pane().child(theme::error(err.clone())),
            Some(Ok(list)) => list,
        };

        let count = list.len();
        let card = theme::card().children(list.iter().enumerate().map(|(index, build)| {
            let label = build.version_name.clone().unwrap_or_else(|| build.build_id.clone());
            let is_current = build.build_id == installed.build_id;
            let busy = self.installing.as_deref() == Some(build.build_id.as_str());
            let row = theme::list_row(("build", index), index + 1 == count)
                .child(div().flex_1().child(label))
                .children(build.date_published.clone().map(theme::muted));
            if is_current {
                row.child(theme::badge(t!("library.properties.releases.current"), false))
            } else {
                let disabled = self.installing.is_some();
                let build_id = build.build_id.clone();
                row.child(
                    button(
                        ("install-build", index),
                        ButtonKind::Secondary,
                        if busy {
                            t!("library.properties.versions.installing")
                        } else {
                            t!("library.properties.versions.install")
                        },
                        disabled,
                    )
                    .when(!disabled, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.rollback(build_id.clone(), cx)))
                    }),
                )
            }
        }));

        pane()
            .child(card)
            .child(theme::muted(t!("library.properties.versions.note")))
            .children(error_line(self.error.as_ref()))
    }
}
