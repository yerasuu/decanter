//! The user's own mod.io API key in Ajustes › General: decanter ships none (decision #1629), so
//! whoever wants mods from mod.io makes one at mod.io and pastes it here. It goes to the keyring
//! and never back into the field; *Probar clave* asks mod.io for one game with it.

use gpui::{Context, Entity, IntoElement, Window, div, prelude::*, px};
use rust_i18n::t;

use super::{column, field_row};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::input::{InputEvent, TextInput};
use crate::widgets::{ButtonKind, button};

pub struct ModioKeySection {
    input: Entity<TextInput>,
    /// Whether a key is saved, `None` until the keyring answered.
    saved: Option<bool>,
    busy: bool,
    status: Option<Text>,
    is_error: bool,
}

impl ModioKeySection {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(Text::new("mods.modio.key.placeholder"), cx).masked());
        cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit) {
                this.save(cx);
            }
        })
        .detach();
        Self::reload(cx);
        Self {
            input,
            saved: None,
            busy: false,
            status: None,
            is_error: false,
        }
    }

    fn reload(cx: &Context<Self>) {
        cx.spawn(async move |this, cx| {
            let read = tokio::task::spawn_blocking(|| crate::modio::api_key().map(|key| key.is_some()))
                .await
                .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                match read {
                    Ok(saved) => this.saved = Some(saved),
                    Err(err) => this.show(err, true),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn show(&mut self, status: Text, is_error: bool) {
        self.status = Some(status);
        self.is_error = is_error;
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let key = self.input.read(cx).text().trim().to_owned();
        if key.is_empty() {
            self.show(Text::new("mods.modio.key.empty"), true);
            cx.notify();
            return;
        }
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = tokio::task::spawn_blocking(move || crate::modio::set_api_key(key))
                .await
                .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.input.update(cx, |input, cx| input.set_text("", cx));
                        this.saved = Some(true);
                        this.show(Text::new("mods.modio.key.saved"), false);
                    }
                    Err(err) => this.show(err, true),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.show(Text::new("mods.modio.key.testing"), false);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let tested = tokio::spawn(async {
                let client = tokio::task::spawn_blocking(crate::modio::client)
                    .await
                    .unwrap_or_else(|err| Err(Text::verbatim(err)))?;
                decanter_modio::probe(&client)
                    .await
                    .map_err(|err| crate::modio::failure(&err))
            })
            .await
            .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match tested {
                    Ok(()) => this.show(Text::new("mods.modio.key.works"), false),
                    Err(err) => {
                        tracing::warn!(error = %err, "modio key check failed");
                        this.show(err, true);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn forget(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = tokio::task::spawn_blocking(crate::modio::forget_api_key)
                .await
                .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        this.saved = Some(false);
                        this.show(Text::new("mods.modio.key.forgotten"), false);
                    }
                    Err(err) => this.show(err, true),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ModioKeySection {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        let saved = self.saved == Some(true);
        let state = match self.saved {
            None => t!("mods.modio.key.reading"),
            Some(true) => t!("mods.modio.key.is_saved"),
            Some(false) => t!("mods.modio.key.none"),
        };
        let status = self.status.as_ref().map(|status| {
            if self.is_error {
                theme::error(status)
            } else {
                theme::muted(status)
            }
        });
        column()
            .child(theme::h2(t!("mods.modio.key.title")))
            .child(
                theme::muted(t!("mods.modio.key.explain", page = decanter_modio::KEY_PAGE))
                    .mb_3()
                    .max_w(px(520.)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .mb(px(12.))
                    .child(
                        button(
                            "modio-key-page",
                            ButtonKind::Secondary,
                            t!("mods.modio.key.open_page"),
                            false,
                        )
                        .on_click(|_, _, _| open_page(decanter_modio::KEY_PAGE)),
                    )
                    .child(
                        button(
                            "modio-key-terms",
                            ButtonKind::Secondary,
                            t!("mods.credits.modio.terms"),
                            false,
                        )
                        .on_click(|_, _, _| open_page(decanter_modio::TERMS_PAGE)),
                    )
                    .child(
                        button(
                            "modio-key-privacy",
                            ButtonKind::Secondary,
                            t!("mods.credits.modio.privacy"),
                            false,
                        )
                        .on_click(|_, _, _| open_page(decanter_modio::PRIVACY_PAGE)),
                    ),
            )
            .child(theme::muted(state).mb_2())
            .child(
                field_row()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(self.input.clone()))
                    .child(
                        button("modio-key-save", ButtonKind::Primary, t!("mods.modio.key.save"), busy)
                            .when(!busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.save(cx)))),
                    ),
            )
            .child(
                field_row()
                    .child(
                        button(
                            "modio-key-test",
                            ButtonKind::Secondary,
                            t!("mods.modio.key.test"),
                            busy || !saved,
                        )
                        .when(!busy && saved, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.test(cx)))
                        }),
                    )
                    .child(
                        button(
                            "modio-key-forget",
                            ButtonKind::Secondary,
                            t!("mods.modio.key.forget"),
                            busy || !saved,
                        )
                        .when(!busy && saved, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.forget(cx)))
                        }),
                    ),
            )
            .children(status)
    }
}

fn open_page(url: &str) {
    if let Err(err) = crate::desktop::open(url) {
        tracing::warn!(%err, "could not open a mod.io page");
    }
}
