//! The PS section: a submenu over everything PS a profile has — its games,
//! the ports that run them, and its settings — in one place rather than
//! spread over Ajustes.

mod adding;
mod branch;
mod defaults;
mod failures;
mod games;
pub mod log;
mod online;
mod package;
mod ports;
mod report;
mod settings;

use decanter_core::capabilities::Feature;
use gpui::{AnyElement, App, Context, Entity, IntoElement, Window, div, prelude::*, px, relative};
use rust_i18n::t;

use crate::capabilities::Caps;
use crate::state::Ps;
use crate::theme;
use failures::FailuresView;
use games::GamesView;
use ports::PortsView;
use settings::SettingsView;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Games,
    Ports,
    Failures,
    Settings,
}

pub struct PsView {
    tab: Tab,
    games: Entity<GamesView>,
    /// Built on first visit, like the shell's own sections.
    ports: Option<Entity<PortsView>>,
    failures: Option<Entity<FailuresView>>,
    settings: Option<Entity<SettingsView>>,
}

impl PsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let ps = Ps::get(cx);
        cx.observe(&ps, |_, _, cx| cx.notify()).detach();
        Self {
            tab: Tab::Games,
            games: cx.new(|cx| GamesView::new(ps, cx)),
            ports: None,
            failures: None,
            settings: None,
        }
    }

    fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        let ps = Ps::get(cx);
        match tab {
            Tab::Games => {}
            Tab::Ports => {
                self.ports.get_or_insert_with(|| cx.new(|cx| PortsView::new(ps, cx)));
            }
            Tab::Failures => {
                self.failures
                    .get_or_insert_with(|| cx.new(|cx| FailuresView::new(ps, cx)));
            }
            Tab::Settings => {
                self.settings
                    .get_or_insert_with(|| cx.new(|cx| SettingsView::new(ps, cx)));
            }
        }
        self.tab = tab;
        cx.notify();
    }
}

impl Render for PsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let items = [
            (Tab::Games, t!("ps.tabs.games")),
            (Tab::Ports, t!("ps.tabs.ports")),
            (Tab::Failures, t!("ps.tabs.failures")),
            (Tab::Settings, t!("ps.tabs.settings")),
        ];
        let tabs = crate::widgets::tabs("ps-tabs", &items, self.tab, move |tab, _, cx| {
            entity.update(cx, |this, cx| this.select(tab, cx));
        })
        .pt_4()
        .px(px(18.));
        let body: AnyElement = match self.tab {
            Tab::Ports => self.ports.clone().map(IntoElement::into_any_element),
            Tab::Failures => self.failures.clone().map(IntoElement::into_any_element),
            Tab::Settings => self.settings.clone().map(IntoElement::into_any_element),
            Tab::Games => None,
        }
        .unwrap_or_else(|| self.games.clone().into_any_element());
        let adding = adding::panel(&Ps::get(cx), cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(tabs)
            .children(adding)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// The section where psport doesn't run yet: the same page, saying why, rather than no section.
pub fn unsupported(cx: &App) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(480.))
                .max_w(relative(0.8))
                .flex()
                .flex_col()
                .items_center()
                .text_center()
                .gap_3()
                .child(theme::h1(t!("ps.unsupported.title")))
                .children(Caps::get(cx).reason(Feature::PsSection).map(theme::muted)),
        )
        .into_any_element()
}
