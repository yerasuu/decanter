//! The owned DLC of an installed Epic game, each installed into or uninstalled from the game's
//! folder on its own, under the action bar: Epic's Propiedades has no DLC tab.

use std::collections::BTreeSet;

use decanter_core::{Game, Storefront};
use gpui::{AnyElement, Context, div, prelude::*, px};
use rust_i18n::t;

use super::DetailView;
use crate::i18n::Text;
use crate::state::{AppState, DownloadKey, DownloadKind, InstallStatus};
use crate::theme;
use crate::views::library::{epic_install, install};
use crate::widgets::{ButtonKind, button};

#[derive(Default)]
pub(super) struct DlcActions {
    /// The DLC being uninstalled.
    busy: Option<String>,
    error: Option<Text>,
}

/// The DLC recorded on the installed Epic game, for the list and the uninstall question.
pub(super) fn installed_dlcs(game: &Game) -> BTreeSet<String> {
    let is_installed = game.install_state.as_ref().is_some_and(|state| state.installed);
    if game.storefront != Storefront::EpicGames || !is_installed {
        return BTreeSet::new();
    }
    crate::core::installed_game(&game.account_id, &game.id)
        .map(|installed| installed.dlcs.into_keys().collect())
        .unwrap_or_default()
}

/// One DLC of the game as the library lists it.
pub(super) struct Dlc {
    pub id: String,
    pub title: String,
    pub is_installable: bool,
}

impl DetailView {
    pub(super) fn epic_dlc_list(&self, game: &Game, dlcs: Vec<Dlc>, cx: &Context<Self>) -> Option<AnyElement> {
        let is_installed = game.install_state.as_ref().is_some_and(|state| state.installed);
        if game.storefront != Storefront::EpicGames || !is_installed || dlcs.is_empty() {
            return None;
        }
        let installed = self.loaded.as_ref().map(|loaded| &loaded.epic_dlcs);
        let actions = &self.local.epic_dlc;
        let background = AppState::get(cx).background.read(cx);
        let count = dlcs.len();
        let rows = dlcs.into_iter().enumerate().map(|(index, dlc)| {
            let is_dlc_installed = installed.is_some_and(|ids| ids.contains(&dlc.id));
            let status = background.status(&game.account_id, &dlc.id);
            let action: AnyElement = if actions.busy.as_deref() == Some(dlc.id.as_str()) {
                theme::muted(t!("epic.dlc.uninstalling")).into_any_element()
            } else if let Some(status) = status.filter(|status| !matches!(status, InstallStatus::Failed(_))) {
                let text = if matches!(status, InstallStatus::Paused { .. }) {
                    t!("epic.dlc.paused")
                } else {
                    t!("epic.dlc.installing")
                };
                theme::muted(text).into_any_element()
            } else if is_dlc_installed {
                let id = dlc.id.clone();
                button(
                    ("epic-dlc-uninstall", index),
                    ButtonKind::Secondary,
                    t!("epic.dlc.uninstall"),
                    actions.busy.is_some(),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.uninstall_epic_dlc(id.clone(), cx)))
                .into_any_element()
            } else if dlc.is_installable {
                let (id, title) = (dlc.id.clone(), dlc.title.clone());
                button(
                    ("epic-dlc-install", index),
                    ButtonKind::Secondary,
                    t!("epic.dlc.install"),
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.install_epic_dlc(id.clone(), title.clone(), cx)))
                .into_any_element()
            } else {
                theme::muted(t!("epic.dlc.no_build")).into_any_element()
            };
            let is_outdated = is_dlc_installed
                && background
                    .outdated_epic_dlcs
                    .contains(&DownloadKey::new(&game.account_id, &dlc.id));
            let failed = match status {
                Some(InstallStatus::Failed(error)) => Some(error.clone()),
                _ => None,
            };
            theme::compact_row(index + 1 == count)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(div().truncate().child(dlc.title))
                        .children(is_outdated.then(|| theme::muted(t!("epic.dlc.update_available")).text_size(px(11.))))
                        .children(failed.map(|error| {
                            theme::muted(t!("library.detail.install.failed", error = error)).text_size(px(11.))
                        })),
                )
                .child(action)
        });
        Some(
            div()
                .mx(px(32.))
                .mt(px(20.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(theme::h3(t!("library.detail.install.dlc")))
                .child(theme::card().children(rows))
                .children(
                    actions
                        .error
                        .as_ref()
                        .map(|error| theme::error(error).into_any_element()),
                )
                .into_any_element(),
        )
    }

    fn install_epic_dlc(&mut self, dlc_id: String, title: String, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let key = DownloadKey::new(&loaded.account_id, &dlc_id);
        tracing::info!(
            account_id = loaded.account_id,
            parent_id = loaded.product_id,
            dlc_id,
            "epic dlc install requested"
        );
        self.local.epic_dlc.error = None;
        let state = AppState::get(cx).clone();
        // On screen before the task runs, so a second click finds the row and does nothing.
        state.background.update(cx, |background, cx| {
            background
                .installing
                .insert(key.clone(), install::queued(title.clone(), DownloadKind::Dlc));
            cx.notify();
        });
        // Detached rather than held by the view: the download outlives the selection.
        cx.spawn(async move |_, cx| {
            epic_install::download(key, title, DownloadKind::Dlc, None, &state, cx).await;
        })
        .detach();
    }

    fn uninstall_epic_dlc(&mut self, dlc_id: String, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let (account_id, parent_id) = (loaded.account_id.clone(), loaded.product_id.clone());
        self.local.epic_dlc = DlcActions {
            busy: Some(dlc_id.clone()),
            error: None,
        };
        cx.notify();
        let state = AppState::get(cx).clone();
        // Detached rather than held by the view: it deletes real files, so selecting another game
        // mid-way mustn't stop it half done.
        cx.spawn(async move |this, cx| {
            let result = epic_install::uninstall_dlc(&account_id, &dlc_id, &parent_id, &state, cx).await;
            let _ = this.update(cx, |this, cx| {
                let Some(loaded) = this.loaded.as_mut() else {
                    return;
                };
                if loaded.product_id != parent_id || loaded.account_id != account_id {
                    return;
                }
                if result.is_ok() {
                    loaded.epic_dlcs.remove(&dlc_id);
                }
                this.local.epic_dlc = DlcActions {
                    busy: None,
                    error: result.err(),
                };
                cx.notify();
            });
        })
        .detach();
    }
}
