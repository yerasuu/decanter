//! GOG's browser login step, shared by adding an account in Ajustes and by
//! renewing a closed session.
//!
//! GOG's `OAuth` has no loopback or device flow: the only way through is a
//! browser login that lands on a page whose URL carries a one-shot code. So
//! the user opens the page, logs in, and pastes the URL they end up on back
//! here. Which account that becomes is only known once GOG answers, so what
//! to do with it is the embedder's call, on [`LoggedIn`].

use gpui::{Context, Entity, EventEmitter, IntoElement, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button};

#[derive(Clone, PartialEq)]
enum Status {
    Idle,
    Working,
    Failed(Text),
}

/// GOG accepted the pasted code and the session is stored under the
/// account's id. The step stays busy until the embedder calls
/// [`GogLogin::reset`] or [`GogLogin::fail`].
pub struct LoggedIn(pub decanter_gog::Account);

pub struct GogLogin {
    pasted: Entity<TextInput>,
    status: Status,
}

impl EventEmitter<LoggedIn> for GogLogin {}

impl GogLogin {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let pasted = cx.new(|cx| TextInput::new("https://embed.gog.com/on_login_success?…&code=…", cx).masked());
        cx.subscribe(&pasted, |_, _, event, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        Self {
            pasted,
            status: Status::Idle,
        }
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.pasted.update(cx, |input, cx| input.set_text("", cx));
        self.status = Status::Idle;
        cx.notify();
    }

    pub fn fail(&mut self, message: Text, cx: &mut Context<Self>) {
        self.status = Status::Failed(message);
        cx.notify();
    }

    fn open_gog(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = crate::desktop::open(decanter_gog::auth_url()) {
            tracing::error!(error = %err, "failed to open browser for gog login");
            self.fail(Text::new("settings.account.errors.browser").arg("error", err), cx);
        }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let code = self.pasted.read(cx).text().to_owned();
        self.status = Status::Working;
        cx.notify();
        tracing::info!("logging in to gog");
        cx.spawn(async move |this, cx| {
            let outcome = decanter_gog::log_in(&code).await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(account) => cx.emit(LoggedIn(account)),
                Err(err) => {
                    tracing::error!(error = %err, "gog login failed");
                    this.fail(Text::verbatim(err), cx);
                }
            });
        })
        .detach();
    }
}

impl Render for GogLogin {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.status == Status::Working || self.pasted.read(cx).text().trim().is_empty();
        // `.card` has no padding of its own; children keep their natural
        // width instead of stretching, as inline buttons did in a block.
        theme::card()
            .items_start()
            .child(theme::muted(t!("settings.account.add.hint")).w_full())
            .child(
                button("open-gog", ButtonKind::Primary, t!("settings.account.add.open"), false)
                    .on_click(cx.listener(|this, _, _, cx| this.open_gog(cx))),
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
                            "connect-gog",
                            ButtonKind::Primary,
                            t!("settings.account.add.connect"),
                            disabled,
                        )
                        .when(!disabled, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.connect(cx)))
                        }),
                    ),
            )
            .children(match &self.status {
                Status::Failed(message) => Some(theme::error(message.clone()).w_full()),
                Status::Idle | Status::Working => None,
            })
    }
}
