//! Amazon Games' browser login step, from the consent notice to the pasted redirect.
//!
//! Like GOG's (`gog_login`), the login is a page the user opens, logs in on, and pastes the
//! address they land on back from. What differs is the notice that comes first, and that the login
//! is single use: a failure spends the [`PendingLogin`], so the user starts over from the button.
//! Which account it becomes is known once Amazon answers, so what to do with it is the embedder's
//! call, on [`LoggedIn`].

use decanter_amazon::PendingLogin;
use gpui::{Context, Entity, EventEmitter, IntoElement, Window, div, prelude::*, px};
use rust_i18n::t;

use super::amazon_consent;
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button};

enum Step {
    Idle,
    Consent,
    Paste(PendingLogin),
    Working,
}

/// Amazon accepted the pasted code and the session is stored under the account's id. The step
/// stays busy until the embedder calls [`AmazonLogin::reset`] or [`AmazonLogin::fail`].
pub struct LoggedIn(pub decanter_amazon::Account);

pub struct AmazonLogin {
    profile_id: String,
    pasted: Entity<TextInput>,
    step: Step,
    failed: Option<Text>,
}

impl EventEmitter<LoggedIn> for AmazonLogin {}

impl AmazonLogin {
    pub fn new(profile_id: String, cx: &mut Context<Self>) -> Self {
        let pasted =
            cx.new(|cx| TextInput::new("https://www.amazon.com/?…openid.oa2.authorization_code=…", cx).masked());
        cx.subscribe(&pasted, |_, _, event, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        Self {
            profile_id,
            pasted,
            step: Step::Idle,
            failed: None,
        }
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.pasted.update(cx, |input, cx| input.set_text("", cx));
        self.step = Step::Idle;
        self.failed = None;
        cx.notify();
    }

    pub fn fail(&mut self, message: Text, cx: &mut Context<Self>) {
        self.pasted.update(cx, |input, cx| input.set_text("", cx));
        self.step = Step::Idle;
        self.failed = Some(message);
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.failed = None;
        if amazon_consent::needs_consent(&self.profile_id) {
            self.step = Step::Consent;
            cx.notify();
        } else {
            self.begin(cx);
        }
    }

    fn accept(&mut self, cx: &mut Context<Self>) {
        match amazon_consent::record_consent(&self.profile_id) {
            Ok(()) => self.begin(cx),
            Err(message) => self.fail(message, cx),
        }
    }

    fn begin(&mut self, cx: &mut Context<Self>) {
        match PendingLogin::new() {
            Ok(pending) => {
                self.step = Step::Paste(pending);
                self.open_sign_in(cx);
                cx.notify();
            }
            Err(err) => {
                tracing::error!(error = %format_args!("{err:#}"), "failed to start the amazon login");
                self.fail(Text::verbatim(format!("{err:#}")), cx);
            }
        }
    }

    fn open_sign_in(&mut self, cx: &mut Context<Self>) {
        let Step::Paste(pending) = &self.step else {
            return;
        };
        if let Err(err) = crate::desktop::open(pending.sign_in_url()) {
            tracing::error!(error = %err, "failed to open browser for amazon login");
            self.failed = Some(Text::new("settings.account.errors.browser").arg("error", err));
            cx.notify();
        }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.step, Step::Paste(_)) {
            return;
        }
        let Step::Paste(pending) = std::mem::replace(&mut self.step, Step::Working) else {
            return;
        };
        let pasted = self.pasted.read(cx).text().to_owned();
        self.failed = None;
        cx.notify();
        tracing::info!("logging in to amazon");
        cx.spawn(async move |this, cx| {
            let outcome = decanter_amazon::complete_login(pending, &pasted).await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(account) => cx.emit(LoggedIn(account)),
                Err(err) => {
                    tracing::error!(error = %format_args!("{err:#}"), "amazon login failed");
                    this.fail(Text::verbatim(format!("{err:#}")), cx);
                }
            });
        })
        .detach();
    }
}

impl Render for AmazonLogin {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let failed = self.failed.clone().map(|message| theme::error(message).w_full());
        let body = match &self.step {
            Step::Idle => div()
                .flex()
                .child(
                    button("add-amazon", ButtonKind::Secondary, t!("amazon.login.button"), false)
                        .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                )
                .into_any_element(),
            Step::Consent => amazon_consent::show_consent(
                "amazon-consent",
                cx.listener(|this, _, _, cx| this.accept(cx)),
                cx.listener(|this, _, _, cx| this.reset(cx)),
            )
            .into_any_element(),
            Step::Paste(_) | Step::Working => {
                let working = matches!(self.step, Step::Working);
                let disabled = working || self.pasted.read(cx).text().trim().is_empty();
                theme::card()
                    .items_start()
                    .child(theme::muted(t!("amazon.login.hint")).w_full())
                    .child(
                        button("open-amazon", ButtonKind::Primary, t!("amazon.login.open"), working)
                            .when(!working, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| this.open_sign_in(cx)))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(10.))
                            .mb(px(12.))
                            .max_w(px(520.))
                            .w_full()
                            .child(div().flex_1().min_w_0().child(self.pasted.clone()))
                            .child(
                                button(
                                    "connect-amazon",
                                    ButtonKind::Primary,
                                    t!("amazon.login.connect"),
                                    disabled,
                                )
                                .when(!disabled, |b| {
                                    b.on_click(cx.listener(|this, _, _, cx| this.connect(cx)))
                                }),
                            )
                            .child(
                                button("cancel-amazon", ButtonKind::Secondary, t!("common.cancel"), working)
                                    .when(!working, |b| b.on_click(cx.listener(|this, _, _, cx| this.reset(cx)))),
                            ),
                    )
                    .into_any_element()
            }
        };
        div().flex().flex_col().gap_2().child(body).children(failed)
    }
}
