//! Epic Games' browser login step, from the consent notice to the pasted code.
//!
//! Like Amazon's (`amazon_login`), the notice comes first and the login is a page the user opens
//! and logs in on. What Epic's page ends on is not an address but a small JSON document holding the
//! code, which the user pastes back whole or as the bare code. Which account it becomes is known
//! once Epic answers, so what to do with it is the embedder's call, on [`LoggedIn`].

use gpui::{Context, Entity, EventEmitter, IntoElement, Window, div, prelude::*, px};
use rust_i18n::t;

use super::epic_consent;
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Idle,
    Consent,
    Paste,
    Working,
}

/// Epic accepted the pasted code and the session is stored under the account's id. The step
/// stays busy until the embedder calls [`EpicLogin::reset`] or [`EpicLogin::fail`].
pub struct LoggedIn(pub decanter_epic::Account);

pub struct EpicLogin {
    profile_id: String,
    pasted: Entity<TextInput>,
    step: Step,
    failed: Option<Text>,
}

impl EventEmitter<LoggedIn> for EpicLogin {}

impl EpicLogin {
    pub fn new(profile_id: String, cx: &mut Context<Self>) -> Self {
        let pasted = cx.new(|cx| TextInput::new("{\"authorizationCode\":\"…\"}", cx).masked());
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

    /// Back to the paste step rather than the button: the login page is still open in the
    /// browser, and reloading it gives a new code.
    pub fn fail(&mut self, message: Text, cx: &mut Context<Self>) {
        self.pasted.update(cx, |input, cx| input.set_text("", cx));
        self.step = Step::Paste;
        self.failed = Some(message);
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.failed = None;
        if epic_consent::needs_consent(&self.profile_id) {
            self.step = Step::Consent;
            cx.notify();
        } else {
            self.begin(cx);
        }
    }

    fn accept(&mut self, cx: &mut Context<Self>) {
        match epic_consent::record_consent(&self.profile_id) {
            Ok(()) => self.begin(cx),
            Err(message) => {
                self.step = Step::Idle;
                self.failed = Some(message);
                cx.notify();
            }
        }
    }

    fn begin(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Paste;
        self.open_login(cx);
        cx.notify();
    }

    fn open_login(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::desktop::open(decanter_epic::login_url()) {
            tracing::error!(error = %err, "failed to open browser for epic login");
            self.failed = Some(Text::new("settings.account.errors.browser").arg("error", err));
            cx.notify();
        }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.step != Step::Paste {
            return;
        }
        self.step = Step::Working;
        let pasted = self.pasted.read(cx).text().to_owned();
        self.failed = None;
        cx.notify();
        tracing::info!("logging in to epic");
        cx.spawn(async move |this, cx| {
            let outcome = decanter_epic::complete_login(&pasted).await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(account) => cx.emit(LoggedIn(account)),
                Err(err) => {
                    tracing::error!(error = %format_args!("{err:#}"), "epic login failed");
                    this.fail(Text::verbatim(format!("{err:#}")), cx);
                }
            });
        })
        .detach();
    }
}

impl Render for EpicLogin {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let failed = self.failed.clone().map(|message| theme::error(message).w_full());
        let body = match self.step {
            Step::Idle => div()
                .flex()
                .child(
                    button("add-epic", ButtonKind::Secondary, t!("epic.login.button"), false)
                        .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                )
                .into_any_element(),
            Step::Consent => epic_consent::show_consent(
                "epic-consent",
                cx.listener(|this, _, _, cx| this.accept(cx)),
                cx.listener(|this, _, _, cx| this.reset(cx)),
            )
            .into_any_element(),
            Step::Paste | Step::Working => {
                let working = self.step == Step::Working;
                let disabled = working || self.pasted.read(cx).text().trim().is_empty();
                theme::card()
                    .items_start()
                    .child(theme::muted(t!("epic.login.hint")).w_full())
                    .child(
                        button("open-epic", ButtonKind::Primary, t!("epic.login.open"), working).when(!working, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.open_login(cx)))
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
                                button("connect-epic", ButtonKind::Primary, t!("epic.login.connect"), disabled)
                                    .when(!disabled, |b| {
                                        b.on_click(cx.listener(|this, _, _, cx| this.connect(cx)))
                                    }),
                            )
                            .child(
                                button("cancel-epic", ButtonKind::Secondary, t!("common.cancel"), working)
                                    .when(!working, |b| b.on_click(cx.listener(|this, _, _, cx| this.reset(cx)))),
                            ),
                    )
                    .into_any_element()
            }
        };
        div().flex().flex_col().gap_2().child(body).children(failed)
    }
}
