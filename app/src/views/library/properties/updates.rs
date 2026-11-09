use std::rc::Rc;

use decanter_core::{InstalledGame, Storefront};
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
    /// The installed Epic DLC the last check found a newer build of.
    dlc_updates: Vec<decanter_epic::DlcUpdate>,
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
            dlc_updates: Vec::new(),
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
        let state = AppState::get(cx).clone();
        self.checking = true;
        self.error = None;
        tracing::debug!(account_id, product_id = %installed.game_id, "checking gog for a newer build");
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = check_build(&account_id, &installed).await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Ok((update, dlc_updates)) => {
                        state.background.update(cx, |background, cx| {
                            for dlc in installed.dlcs.keys() {
                                let key = DownloadKey::new(&account_id, dlc);
                                if dlc_updates.iter().any(|update| update.app_name == *dlc) {
                                    background.outdated_epic_dlcs.insert(key);
                                } else {
                                    background.outdated_epic_dlcs.remove(&key);
                                }
                            }
                            cx.notify();
                        });
                        this.result = Some(update);
                        this.dlc_updates = dlc_updates;
                    }
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
        let latest = self.result.clone().flatten();
        let dlc_updates = self.dlc_updates.clone();
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
            let is_amazon = cfg!(target_os = "linux") && target.storefront == Storefront::AmazonGames;
            if is_amazon || target.storefront == Storefront::EpicGames {
                let key = DownloadKey::new(account_id, product_id);
                let title = target.title.clone();
                let mut remaining = dlc_updates;
                if target.storefront == Storefront::EpicGames {
                    let is_game_current = latest.is_none()
                        || crate::views::library::epic_install::download(
                            key,
                            title,
                            DownloadKind::Update,
                            None,
                            &state,
                            cx,
                        )
                        .await;
                    // The DLC build is asked for the game's folder as it now is, so a failed game
                    // update leaves them for the next try.
                    if is_game_current {
                        let pending = std::mem::take(&mut remaining);
                        for dlc in pending {
                            let key = DownloadKey::new(account_id, &dlc.app_name);
                            let is_done = crate::views::library::epic_install::download(
                                key,
                                dlc.title.clone(),
                                DownloadKind::Dlc,
                                None,
                                &state,
                                cx,
                            )
                            .await;
                            if !is_done {
                                remaining.push(dlc);
                            }
                        }
                    }
                } else {
                    #[cfg(target_os = "linux")]
                    crate::views::library::amazon_install::download(key, title, DownloadKind::Update, None, &state, cx)
                        .await;
                }
                let _ = this.update(cx, |this, cx| {
                    this.updating = false;
                    this.reload();
                    // The row in Descargas carries a failure or a pause; the tab only learns
                    // whether the record moved to the version the check found.
                    let is_current = this
                        .installed
                        .as_ref()
                        .is_some_and(|installed| Some(&installed.build_id) == latest.as_ref());
                    if is_current || latest.is_none() {
                        this.result = Some(None);
                    }
                    this.dlc_updates = remaining;
                    cx.notify();
                });
                return;
            }
            let (on_progress, progress) = install::progress_channel();
            let update = decanter_gog::install(
                &core_library,
                account_id,
                product_id,
                None,
                &target.title,
                None,
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
                    Ended::Cancelled | Ended::Paused { .. } => {}
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
        let update_label = if self.updating {
            t!("library.properties.updates.updating")
        } else {
            t!("library.properties.updates.update")
        };
        let dlc_titles = self
            .dlc_updates
            .iter()
            .map(|dlc| dlc.title.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        match &self.result {
            Some(None) if !self.dlc_updates.is_empty() => {
                actions = actions
                    .child(theme::muted(t!(
                        "library.properties.updates.dlc_available",
                        dlcs = dlc_titles
                    )))
                    .child(
                        button("install-update", ButtonKind::Primary, update_label, self.updating)
                            .when(!self.updating, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| this.install_update(cx)))
                            }),
                    );
            }
            Some(Some(build_id)) => {
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
                    .children(
                        (!matches!(installed.storefront, Storefront::AmazonGames | Storefront::EpicGames)).then(|| {
                            row(
                                t!("library.detail.install.branch"),
                                installed.branch.clone().unwrap_or_else(|| "mainline".to_string()),
                            )
                        }),
                    ),
            )
            .child(actions)
            .children(error_line(self.error.as_ref()))
    }
}

/// The newer build's id for `installed`, from its own store, and the Epic DLC with a newer build.
async fn check_build(
    account_id: &str,
    installed: &InstalledGame,
) -> anyhow::Result<(Option<String>, Vec<decanter_epic::DlcUpdate>)> {
    #[cfg(target_os = "linux")]
    if installed.storefront == Storefront::AmazonGames {
        return Ok((decanter_amazon::check_update(account_id, installed).await?, Vec::new()));
    }
    if installed.storefront == Storefront::EpicGames {
        let updates = decanter_epic::check_updates(account_id, installed).await?;
        return Ok((updates.game, updates.dlcs));
    }
    Ok((decanter_gog::check_update(account_id, installed).await?, Vec::new()))
}
