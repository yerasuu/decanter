//! Which DLC are installed alongside this game — Steam's own Properties →
//! DLC, applied as you go (ticking installs, unticking uninstalls) rather
//! than staged for later. What a *first* install of the base game fetches
//! alongside it is a different moment, decided in the install dialog
//! before the game exists at all; this tab is purely post-install
//! management, the same way Steam's is.

use std::rc::Rc;

use decanter_core::{CachedGame, InstalledGame};
use gpui::{Context, Window, div, prelude::*};
use rust_i18n::t;

use super::{Target, error_line, not_installed, pane};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::checkbox;

pub(super) struct DlcTab {
    target: Rc<Target>,
    installed: Option<InstalledGame>,
    dlcs: Vec<CachedGame>,
    goodies: Vec<CachedGame>,
    busy: Option<String>,
    error: Option<Text>,
}

impl DlcTab {
    pub(super) fn new(target: Rc<Target>) -> Self {
        let mut this = Self {
            target,
            installed: None,
            dlcs: Vec::new(),
            goodies: Vec::new(),
            busy: None,
            error: None,
        };
        this.reload();
        this
    }

    fn reload(&mut self) {
        let account_id = self.target.account_id.as_str();
        let product_id = self.target.product_id.as_str();
        self.installed = crate::core::installed_game(account_id, product_id);
        // Read from the cached catalog rather than the live library: this
        // window reads none of the main window's entities (see the module
        // doc), and a plain cache read is all a DLC list needs.
        // GOG files an artbook and a playable expansion under the same `dlc`
        // type, but a goodie has no installer and no depot in the game's
        // build — ticking one could only ever fail, so it isn't offered.
        (self.dlcs, self.goodies) = crate::core::db()
            .inspect_err(|err| tracing::warn!(account_id, error = %err, "failed to open db, dlc list is empty"))
            .ok()
            .and_then(|db| {
                db.list_games(account_id)
                    .inspect_err(
                        |err| tracing::warn!(account_id, error = %err, "failed to list cached games for dlc tab"),
                    )
                    .ok()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.parent_id.as_deref() == Some(product_id))
            .partition(|entry| entry.installable);
    }

    fn toggle(&mut self, dlc: &CachedGame, on: bool, cx: &mut Context<Self>) {
        let target = Rc::clone(&self.target);
        let dlc_id = dlc.product_id.clone();
        let title = dlc.title.clone();
        self.error = None;
        self.busy = Some(dlc_id.clone());
        tracing::info!(
            account_id = target.account_id,
            parent_id = target.product_id,
            dlc_id,
            on,
            "dlc install state toggled"
        );
        cx.notify();
        cx.spawn(async move |this, cx| {
            let account_id = target.account_id.as_str();
            let parent_id = target.product_id.as_str();
            let Ok(core_library) = crate::core::library_for_account(account_id) else {
                tracing::error!(
                    account_id,
                    parent_id,
                    dlc_id,
                    "could not resolve library, dlc toggle aborted"
                );
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(Text::new("library.install.no_library"));
                    this.busy = None;
                    cx.notify();
                });
                return;
            };
            let outcome = if on {
                decanter_gog::install(
                    &core_library,
                    account_id,
                    &dlc_id,
                    Some(parent_id),
                    &title,
                    None,
                    None,
                    |_| {},
                )
                .await
            } else {
                decanter_gog::uninstall_dlc(&core_library, account_id, &dlc_id, parent_id).await
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    Ok(()) => this.reload(),
                    Err(err) => {
                        tracing::error!(account_id, dlc_id, on, error = %err, "dlc install state change failed");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for DlcTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(installed) = &self.installed else {
            return not_installed();
        };
        if self.dlcs.is_empty() && self.goodies.is_empty() {
            return pane().child(theme::muted(t!("library.properties.dlc.none")));
        }

        let dlc_count = self.dlcs.len();
        let dlc_card = (!self.dlcs.is_empty()).then(|| {
            theme::card().children(self.dlcs.iter().enumerate().map(|(index, dlc)| {
                let checked = installed.dlcs.contains_key(&dlc.product_id);
                let is_busy = self.busy.as_deref() == Some(dlc.product_id.as_str());
                let entry = dlc.clone();
                // The whole row toggles, since the checkbox sat inside a
                // `label` in the webview build.
                theme::list_row(("dlc", index), index + 1 == dlc_count)
                    .when(!is_busy, |row| {
                        row.on_click(cx.listener(move |this, _, _, cx| this.toggle(&entry, !checked, cx)))
                    })
                    .child(checkbox(checked).when(is_busy, |b| b.opacity(0.4)))
                    .child(div().flex_1().child(dlc.title.clone()))
                    .children(is_busy.then(|| theme::muted(t!("library.properties.dlc.applying"))))
            }))
        });

        let goodie_count = self.goodies.len();
        let extras = (!self.goodies.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(theme::h3(t!("library.sidebar.kind.extras")))
                .child(theme::muted(t!("library.properties.dlc.goodies_note")))
                .child(
                    theme::card().children(self.goodies.iter().enumerate().map(|(index, goodie)| {
                        theme::list_row(("goodie", index), index + 1 == goodie_count)
                            .child(div().flex_1().child(goodie.title.clone()))
                            .child(theme::muted(t!("library.properties.dlc.not_installable")))
                    })),
                )
        });

        pane()
            .children(dlc_card)
            .children(error_line(self.error.as_ref()))
            .children(extras)
    }
}
