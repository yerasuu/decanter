//! The passphrase prompt for the secrets file, shown once per run when the system keyring is
//! missing or stays locked (decision #1307): unlock the file that is there, or create one,
//! asking for the new passphrase twice. «Ahora no» leaves secrets unsaved for this run, as
//! before there was a file.

use decanter_core::secrets::{self, FileError, FileOffer, Secret};
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

/// The prompt is done with: the file is the store now, or the user put it off.
pub struct Closed {
    pub is_unlocked: bool,
}

pub struct PassphrasePrompt {
    offer: FileOffer,
    passphrase: Entity<TextInput>,
    repeated: Entity<TextInput>,
    status: Status,
}

impl EventEmitter<Closed> for PassphrasePrompt {}

impl PassphrasePrompt {
    pub fn new(offer: FileOffer, cx: &mut Context<Self>) -> Self {
        let passphrase = cx.new(|cx| TextInput::new(Text::new("chrome.passphrase.placeholder"), cx).masked());
        let repeated = cx.new(|cx| TextInput::new(Text::new("chrome.passphrase.repeat"), cx).masked());
        for input in [&passphrase, &repeated] {
            cx.subscribe(input, |this, _, event, cx| match event {
                InputEvent::Submit => this.submit(cx),
                InputEvent::Cancel => this.dismiss(cx),
                InputEvent::Changed => cx.notify(),
            })
            .detach();
        }
        Self {
            offer,
            passphrase,
            repeated,
            status: Status::Idle,
        }
    }

    fn dismiss(&self, cx: &mut Context<Self>) {
        if self.status == Status::Working {
            return;
        }
        self.clear(cx);
        tracing::info!(offer = ?self.offer, "secrets file prompt put off, secrets not saved this run");
        cx.emit(Closed { is_unlocked: false });
    }

    fn clear(&self, cx: &mut Context<Self>) {
        for input in [&self.passphrase, &self.repeated] {
            input.update(cx, |input, cx| input.set_text("", cx));
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.status == Status::Working || self.passphrase.read(cx).text().is_empty() {
            return;
        }
        let creating = self.offer == FileOffer::Create;
        if creating && self.passphrase.read(cx).text() != self.repeated.read(cx).text() {
            self.repeated.update(cx, |input, cx| input.set_text("", cx));
            self.status = Status::Failed(Text::new("chrome.passphrase.mismatch"));
            cx.notify();
            return;
        }
        let passphrase = Secret::from(self.passphrase.read(cx).text().to_owned());
        self.clear(cx);
        self.status = Status::Working;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_spawn(async move {
                    if creating {
                        secrets::store().create_file(&passphrase)
                    } else {
                        secrets::store().unlock_file(&passphrase)
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| match outcome {
                Ok(()) => cx.emit(Closed { is_unlocked: true }),
                Err(err) => {
                    tracing::warn!(error = %err, creating, "secrets file not opened");
                    this.status = Status::Failed(match err {
                        FileError::WrongPassphrase => Text::new("chrome.passphrase.wrong"),
                        other => Text::new("chrome.passphrase.failed").arg("error", other),
                    });
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

impl Render for PassphrasePrompt {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let creating = self.offer == FileOffer::Create;
        let working = self.status == Status::Working;
        let disabled = working
            || self.passphrase.read(cx).text().is_empty()
            || (creating && self.repeated.read(cx).text().is_empty());
        let (title, body, action) = if creating {
            (
                t!("chrome.passphrase.create_title"),
                t!("chrome.passphrase.create_body"),
                t!("chrome.passphrase.create"),
            )
        } else {
            (
                t!("chrome.passphrase.unlock_title"),
                t!("chrome.passphrase.unlock_body"),
                t!("chrome.passphrase.unlock"),
            )
        };
        let viewport = window.viewport_size();
        theme::card()
            .w(px(480.).min(viewport.width * 0.92))
            .p_6()
            .gap_3()
            .child(theme::display(16.).child(title.to_uppercase()))
            .child(theme::muted(body))
            .child(self.passphrase.clone())
            .when(creating, |card| {
                card.child(self.repeated.clone())
                    .child(theme::muted(t!("chrome.passphrase.no_recovery")))
            })
            .children(match &self.status {
                Status::Failed(message) => Some(theme::error(message.clone())),
                Status::Idle | Status::Working => None,
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(10.))
                    .child(
                        button(
                            "passphrase-later",
                            ButtonKind::Secondary,
                            t!("chrome.passphrase.later"),
                            working,
                        )
                        .when(!working, |b| b.on_click(cx.listener(|this, _, _, cx| this.dismiss(cx)))),
                    )
                    .child(
                        button("passphrase-submit", ButtonKind::Primary, action, disabled)
                            .when(!disabled, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit(cx)))),
                    ),
            )
    }
}
