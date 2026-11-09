//! The user's personal Nexus Mods API key in Ajustes › General: saved to the keyring, never shown
//! back, checked against Nexus only when the user presses *Probar clave*, and forgotten on
//! *Borrar clave*. decanter has no key of its own (decision #1629).

use decanter_core::secrets::Secret;
use decanter_nexus::Account;
use gpui::{Context, Entity, IntoElement, Window, div, prelude::*};
use rust_i18n::t;

use super::{column, field_row};
use crate::i18n::Text;
use crate::nexus::{account_line, failure_text};
use crate::theme;
use crate::widgets::input::TextInput;
use crate::widgets::{ButtonKind, button};

/// Where a personal key is made: the account's API access page on the site.
const KEY_PAGE: &str = "https://www.nexusmods.com/users/myaccount?tab=api";

pub struct NexusSection {
    input: Entity<TextInput>,
    /// `None` while the keyring is read.
    is_saved: Option<bool>,
    busy: bool,
    status: Option<Text>,
    account: Option<Account>,
}

impl NexusSection {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(Text::new("mods.nexus.settings.placeholder"), cx).masked());
        // A keyring round trip, off the UI thread; the key itself never goes back into the field.
        cx.spawn(async move |this, cx| {
            let loaded = tokio::task::spawn_blocking(decanter_nexus::load_key).await;
            let _ = this.update(cx, |this, cx| {
                this.is_saved = Some(match loaded {
                    Ok(Ok(key)) => key.is_some(),
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "could not read the nexus key from the keyring");
                        this.status = Some(failure_text(&err));
                        false
                    }
                    Err(err) => {
                        tracing::error!(%err, "reading the nexus key stopped");
                        false
                    }
                });
                cx.notify();
            });
        })
        .detach();
        Self {
            input,
            is_saved: None,
            busy: false,
            status: None,
            account: None,
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let typed = self.input.read(cx).text().trim().to_owned();
        if typed.is_empty() {
            self.status = Some(Text::new("mods.nexus.settings.empty"));
            cx.notify();
            return;
        }
        self.busy = true;
        self.account = None;
        self.status = Some(Text::new("mods.nexus.settings.saving"));
        cx.notify();
        let key = Secret::from(typed);
        cx.spawn(async move |this, cx| {
            let saved = tokio::task::spawn_blocking(move || decanter_nexus::save_key(&key)).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.status = Some(match saved {
                    Ok(Ok(())) => {
                        this.is_saved = Some(true);
                        this.input.update(cx, |input, cx| input.set_text("", cx));
                        Text::new("mods.nexus.settings.saved")
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "nexus key not saved");
                        failure_text(&err)
                    }
                    Err(err) => Text::verbatim(err),
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.account = None;
        self.status = Some(Text::new("mods.nexus.settings.testing"));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let checked = tokio::spawn(async { decanter_nexus::validate(&crate::nexus::client().await?).await }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match checked {
                    Ok(Ok(account)) => {
                        this.status = None;
                        this.account = Some(account);
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, retryable = err.is_retryable(), "nexus key check failed");
                        this.status = Some(failure_text(&err));
                    }
                    Err(err) => {
                        tracing::error!(%err, "nexus key check stopped");
                        this.status = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn forget(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.account = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let forgotten = tokio::task::spawn_blocking(decanter_nexus::forget_key).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.status = Some(match forgotten {
                    Ok(Ok(())) => {
                        this.is_saved = Some(false);
                        Text::new("mods.nexus.settings.forgotten")
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, "nexus key not forgotten");
                        failure_text(&err)
                    }
                    Err(err) => Text::verbatim(err),
                });
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for NexusSection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_saved = self.is_saved == Some(true);
        let saved_line = match self.is_saved {
            None => t!("mods.nexus.settings.loading"),
            Some(true) => t!("mods.nexus.settings.is_saved"),
            Some(false) => t!("mods.nexus.settings.none"),
        };
        let can_use = is_saved && !self.busy;
        column()
            .child(theme::h2(t!("mods.nexus.settings.title")))
            .child(theme::muted(t!("mods.nexus.settings.hint")))
            .child(
                field_row()
                    .mt_2()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(self.input.clone()))
                    .child(
                        button(
                            "nexus-key-save",
                            ButtonKind::Secondary,
                            t!("mods.nexus.settings.save"),
                            self.busy,
                        )
                        .when(!self.busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.save(cx)))),
                    )
                    .child(
                        button(
                            "nexus-key-page",
                            ButtonKind::Icon,
                            t!("mods.nexus.settings.get_key"),
                            false,
                        )
                        .on_click(|_, _, _| {
                            if let Err(err) = crate::desktop::open(KEY_PAGE) {
                                tracing::warn!(%err, "could not open the nexus api key page");
                            }
                        }),
                    ),
            )
            .child(theme::muted(saved_line))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_2()
                    .child(
                        button(
                            "nexus-key-test",
                            ButtonKind::Secondary,
                            t!("mods.nexus.settings.test"),
                            !can_use,
                        )
                        .when(can_use, |b| b.on_click(cx.listener(|this, _, _, cx| this.test(cx)))),
                    )
                    .child(
                        button(
                            "nexus-key-forget",
                            ButtonKind::Secondary,
                            t!("mods.nexus.settings.forget"),
                            !can_use,
                        )
                        .when(can_use, |b| b.on_click(cx.listener(|this, _, _, cx| this.forget(cx)))),
                    ),
            )
            .children(
                self.account
                    .as_ref()
                    .map(|account| div().mt_2().child(account_line(account))),
            )
            .children(self.status.as_ref().map(|status| theme::muted(status).mt_2()))
    }
}
