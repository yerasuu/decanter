//! Ajustes' defaults for new games: what a game seen for the first time copies, once, into its own
//! settings, so a change here reaches no game that already has them (`docs/ps-per-game-settings.md`).
//! The Graphics rows and the online switches are edited here; everything else a game has (keyboard,
//! pad layout, extras) comes in by taking a game's settings as the defaults.

use decanter_core::capabilities::Feature;
use decanter_ps::bbport::LauncherSettings;
use decanter_ps::bbport::online;
use decanter_ps::bbport::per_game::{self, Category, Target};
use gpui::{AppContext as _, Context, Entity, SharedString, Window, div, prelude::*};
use rust_i18n::t;

use super::games::{GraphicsView, display_title};
use crate::capabilities::Caps;
use crate::i18n::Text;
use crate::state::{Ps, per_game_error};
use crate::theme;
use crate::widgets::{ButtonKind, Select, button, checkbox, confirm_panel, select::SelectStyle};

/// `(key, label, value when absent)`, as each game's Online tab reads them: absent, the plain-text
/// login is allowed (#1303) and `online_upnp` is on.
const SWITCHES: [(&str, &str, bool); 4] = [
    (online::ONLINE, "ps.online.play", false),
    (online::UPNP, "ps.online.upnp", true),
    (online::TLS, "ps.online.tls.label", false),
    (online::ALLOW_PLAINTEXT, "ps.online.plaintext.label", true),
];

pub struct DefaultsView {
    ps: Entity<Ps>,
    graphics: Entity<GraphicsView>,
    /// One per [`SWITCHES`].
    switches: [bool; 4],
    /// Titles with settings of their own, to take the defaults from.
    sources: Vec<String>,
    source: Option<String>,
    is_confirming: bool,
    message: Option<(Text, bool)>,
}

impl DefaultsView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        let graphics = cx.new(|cx| GraphicsView::new(&ps, cx));
        graphics.update(cx, GraphicsView::show_defaults);
        let this = Self {
            ps,
            graphics,
            switches: SWITCHES.map(|(_, _, absent)| absent),
            sources: Vec::new(),
            source: None,
            is_confirming: false,
            message: None,
        };
        this.reload(cx);
        this
    }

    fn reload(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let (defaults, titles) = cx
                .background_spawn(async {
                    let root = decanter_ps::paths::active();
                    (LauncherSettings::defaults(&root), per_game::titles(&root))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match defaults {
                    Ok(defaults) => this.switches = SWITCHES.map(|(key, _, absent)| defaults.flag(key, absent)),
                    Err(err) => this.message = Some((Text::new("ps.defaults.read_error").arg("error", err), true)),
                }
                if this.source.as_ref().is_none_or(|source| !titles.contains(source)) {
                    this.source = titles.first().cloned();
                }
                this.sources = titles;
                cx.notify();
            });
        })
        .detach();
    }

    fn set_switch(&mut self, index: usize, is_on: bool, cx: &mut Context<Self>) {
        let Some(&(key, _, _)) = SWITCHES.get(index) else {
            return;
        };
        if let Some(switch) = self.switches.get_mut(index) {
            *switch = is_on;
        }
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move {
                    LauncherSettings::edit_defaults(&decanter_ps::paths::active(), |defaults| {
                        defaults.set_flag(key, is_on);
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.message = saved
                    .err()
                    .map(|err| (Text::new("ps.defaults.save_error").arg("error", err), true));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn take_from_source(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        self.is_confirming = false;
        let name = self.name(&source, cx);
        cx.spawn(async move |this, cx| {
            let copied = cx
                .background_spawn(async move {
                    let root = decanter_ps::paths::active();
                    let from = Target::parse(&root, &source)?;
                    per_game::copy(&root, &from, &Target::Defaults, &Category::ALL)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.message = Some(match copied {
                    Ok(backup) => (
                        Text::new("ps.per_game.done_with_backup")
                            .text("done", Text::new("ps.defaults.taken").arg("game", name))
                            .arg("path", backup.display()),
                        false,
                    ),
                    Err(err) => {
                        tracing::warn!(error = %err, "defaults not taken from a game");
                        (per_game_error(&err), true)
                    }
                });
                this.graphics.update(cx, GraphicsView::show_defaults);
                this.reload(cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn name(&self, title_id: &str, cx: &Context<Self>) -> String {
        self.ps
            .read(cx)
            .entries()
            .iter()
            .find(|entry| entry.game.title_id == title_id)
            .map_or_else(|| title_id.to_owned(), |entry| display_title(&entry.game))
    }

    fn online_switches(&self, cx: &Context<Self>) -> impl IntoElement {
        let count = SWITCHES.len();
        theme::card().children(SWITCHES.iter().zip(self.switches).enumerate().map(
            |(index, (&(_, label, _), is_on))| {
                theme::compact_row(index + 1 == count).child(
                    div()
                        .id(("ps-defaults-switch", index))
                        .flex()
                        .items_center()
                        .gap_3()
                        .cursor_pointer()
                        .child(checkbox(is_on))
                        .child(SharedString::from(t!(label)))
                        .on_click(cx.listener(move |this, _, _, cx| this.set_switch(index, !is_on, cx))),
                )
            },
        ))
    }

    fn take_row(&self, cx: &Context<Self>) -> impl IntoElement {
        if self.sources.is_empty() {
            return theme::card()
                .child(theme::compact_row(true).child(theme::muted(t!("ps.per_game.no_sources"))))
                .into_any_element();
        }
        let names = self.sources.iter().map(|source| {
            (
                SharedString::from(source.clone()),
                SharedString::from(self.name(source, cx)),
            )
        });
        let entity = cx.entity();
        let select = Select::new(
            "ps-defaults-source",
            names,
            SharedString::from(self.source.clone().unwrap_or_default()),
        )
        .style(SelectStyle::Compact)
        .on_change(move |value, _, cx| {
            let value = value.to_string();
            entity.update(cx, |this, cx| {
                this.source = Some(value);
                cx.notify();
            });
        });
        theme::card()
            .child(
                theme::compact_row(true)
                    .gap_3()
                    .child(div().flex_1().child(SharedString::from(t!("ps.defaults.take_label"))))
                    .child(select)
                    .child(
                        button("ps-defaults-take", ButtonKind::Secondary, t!("ps.defaults.take"), false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.is_confirming = true;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for DefaultsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let confirm = self.is_confirming.then(|| {
            let (yes, no) = (cx.entity(), cx.entity());
            let game = self
                .source
                .as_deref()
                .map(|source| self.name(source, cx))
                .unwrap_or_default();
            confirm_panel(
                "ps-defaults-take-confirm",
                t!("ps.defaults.confirm.title"),
                t!("ps.defaults.confirm.body", game = game),
                t!("ps.defaults.confirm.yes"),
                move |_, _, cx| yes.update(cx, Self::take_from_source),
                move |_, _, cx| {
                    no.update(cx, |this, cx| {
                        this.is_confirming = false;
                        cx.notify();
                    });
                },
            )
        });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(theme::h2(t!("ps.defaults.title")))
            .child(theme::muted(t!("ps.defaults.description")))
            .child(self.take_row(cx))
            .child(theme::muted(t!("ps.defaults.take_note")))
            .children(confirm)
            .children(self.message.as_ref().map(|(text, is_error)| {
                if *is_error {
                    theme::error(text)
                } else {
                    theme::muted(text)
                }
            }))
            .when(!Caps::get(cx).is_never_here(Feature::PsOnline), |page| {
                page.child(theme::h3(t!("ps.defaults.online_title")))
                    .child(self.online_switches(cx))
            })
            .child(theme::h3(t!("ps.defaults.graphics_title")))
            .child(self.graphics.clone())
    }
}
