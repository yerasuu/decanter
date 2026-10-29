use std::rc::Rc;

use decanter_core::InstalledGame;
use gpui::{Context, Window, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, field_row, not_installed, pane, row, rows};
use crate::i18n::Text;
use crate::state::{AppState, DownloadKey, DownloadKind};
use crate::theme;
use crate::views::library::install::{self, DownloadJob, Ended};
use crate::widgets::{ButtonKind, button};

pub(super) struct UpdatesTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    checking: bool,
    /// `None` until a check has run; then the newer build's id, or `None`
    /// inside for "already up to date".
    result: Option<Option<String>>,
    updating: bool,
    error: Option<Text>,
}

impl UpdatesTab {
    pub(super) fn new(target: Rc<Target>) -> Self {
        let installed = crate::core::installed_game(&target.account_id, &target.product_id);
        Self {
            target,
            installed,
            checking: false,
            result: None,
            updating: false,
            error: None,
        }
    }

    fn reload(&mut self) {
        self.installed = crate::core::installed_game(&self.target.account_id, &self.target.product_id);
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        let Some(installed) = self.installed.clone() else {
            return;
        };
        let account_id = self.target.account_id.clone();
        self.checking = true;
        self.error = None;
        tracing::debug!(account_id, product_id = %installed.game_id, "checking gog for a newer build");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = decanter_gog::check_update(&account_id, &installed).await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok(update) => this.result = Some(update),
                    Err(err) => {
                        tracing::error!(account_id, product_id = %installed.game_id, error = %err, "update check failed");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                this.checking = false;
                this.reload();
                cx.notify();
            });
        })
        .detach();
    }

    fn install_update(&mut self, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        self.updating = true;
        self.error = None;
        tracing::info!(
            account_id = target.account_id,
            product_id = target.product_id,
            "update install started"
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
                    "could not resolve library, update install aborted"
                );
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(Text::new("library.install.no_library"));
                    this.updating = false;
                    cx.notify();
                });
                return;
            };
            let (on_progress, progress) = install::progress_channel();
            let update = decanter_gog::install(
                &core_library,
                account_id,
                product_id,
                None,
                &target.title,
                None,
                None,
                on_progress,
            );
            let job = DownloadJob {
                key: DownloadKey::new(account_id, product_id),
                title: target.title.clone(),
                kind: DownloadKind::Update,
            };
            let outcome = install::track(&state, job, progress, update, cx).await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ended::Done(()) => {
                        tracing::info!(account_id, product_id, "update install finished");
                        this.result = Some(None);
                    }
                    Ended::Cancelled => {}
                    Ended::Failed(error) => this.error = Some(error),
                }
                this.updating = false;
                this.reload();
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for UpdatesTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(installed) = &self.installed else {
            return not_installed();
        };

        let check_label = if self.checking {
            t!("library.properties.updates.checking")
        } else {
            t!("library.properties.updates.check")
        };
        let mut actions = field_row().child(
            button("check-update", ButtonKind::Secondary, check_label, self.checking).when(!self.checking, |b| {
                b.on_click(cx.listener(|this, _, _, cx| this.check(cx)))
            }),
        );
        match &self.result {
            Some(Some(build_id)) => {
                let update_label = if self.updating {
                    t!("library.properties.updates.updating")
                } else {
                    t!("library.properties.updates.update")
                };
                actions = actions
                    .child(theme::muted(t!(
                        "library.properties.updates.available",
                        build = build_id
                    )))
                    .child(
                        button("install-update", ButtonKind::Primary, update_label, self.updating)
                            .when(!self.updating, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| this.install_update(cx)))
                            }),
                    );
            }
            Some(None) => actions = actions.child(theme::muted(t!("library.properties.updates.up_to_date"))),
            None => {}
        }

        pane()
            .child(
                rows()
                    .child(row(
                        t!("library.properties.updates.installed_build"),
                        installed.build_id.clone(),
                    ))
                    .child(row(
                        t!("library.detail.install.branch"),
                        installed.branch.clone().unwrap_or_else(|| "mainline".to_string()),
                    )),
            )
            .child(actions)
            .children(error_line(self.error.as_ref()))
    }
}
