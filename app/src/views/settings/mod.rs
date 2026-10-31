//! decanter's own settings, the last section of the main navigation.
//! A sidebar of categories beside the chosen one's content: window behaviour
//! and language (first, since it's where Ajustes opens), downloads and
//! updates, the Steam and Galaxy integrations with the mod provider keys, the
//! profile's accounts (with where its library lives, which library is active
//! and adding another person), the default Proton every game runs on unless
//! it pins one of its own (global, not tied to any profile; not on Windows,
//! where games run natively), and the advanced ones. PS settings stay in the
//! PS page, which the sidebar links to. External games are added from
//! Biblioteca instead (see `crate::views::external`).

mod account;
mod compatibility;
mod credentials;
mod debug;
mod game_tools;
mod general;
mod modio;
mod nexus;
mod window_display;

use std::borrow::Cow;

use decanter_core::Storefront;
use decanter_core::capabilities::Feature;
use gpui::{App, Context, Div, Entity, FontWeight, IntoElement, Stateful, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::capabilities::Caps;
use crate::state::AppState;
use crate::theme;
use crate::widgets::reveal::reveal;
use account::AccountTab;
use compatibility::CompatibilityTab;
use general::GeneralTab;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Downloads,
    Integrations,
    Account,
    Compatibility,
    Advanced,
}

impl Tab {
    fn label(self) -> Cow<'static, str> {
        match self {
            Self::General => t!("settings.tabs.general"),
            Self::Downloads => t!("settings.tabs.downloads"),
            Self::Integrations => t!("settings.tabs.integrations"),
            Self::Account => t!("settings.tabs.account"),
            Self::Compatibility => t!("settings.tabs.compatibility"),
            Self::Advanced => t!("settings.tabs.advanced"),
        }
    }
}

/// Asks the shell to show Ajustes on `tab`.
pub fn open(tab: Tab, cx: &mut App) {
    let session = AppState::get(cx).session.clone();
    session.update(cx, |session, cx| {
        session.settings_request = Some(tab);
        cx.notify();
    });
}

/// Asks the shell to show the PS page, whose settings live there.
fn open_ps(cx: &mut App) {
    let session = AppState::get(cx).session.clone();
    session.update(cx, |session, cx| {
        session.ps_request = session.ps_request.wrapping_add(1);
        cx.notify();
    });
}

/// The open tab's own entity. Rebuilt on every tab switch or profile
/// change, so a tab's local state (a half-typed GOG URL, a session check)
/// starts over the way a remounted component's did in the webview build.
enum Body {
    General(Entity<GeneralTab>),
    Account(Entity<AccountTab>),
    Compatibility(Entity<CompatibilityTab>),
}

pub struct SettingsView {
    tab: Tab,
    profile: Option<String>,
    body: Option<Body>,
}

impl SettingsView {
    pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let session = AppState::get(cx).session.clone();
        cx.observe(&session, |this, _, cx| this.sync_profile(cx)).detach();
        let mut this = Self {
            tab: Tab::General,
            profile: None,
            body: None,
        };
        this.sync_profile(cx);
        this
    }

    fn sync_profile(&mut self, cx: &mut Context<Self>) {
        let session = AppState::get(cx).session.read(cx);
        let profile = session.core.is_ok().then(|| session.active_profile.clone()).flatten();
        if profile == self.profile && (profile.is_none() || self.body.is_some()) {
            return;
        }
        self.profile = profile;
        self.rebuild(cx);
    }

    /// Switches to the tab the session asks for, if any, and clears the request.
    pub fn open_requested(&mut self, cx: &mut Context<Self>) {
        let session = AppState::get(cx).session.clone();
        let requested = session.update(cx, |session, _| session.settings_request.take());
        if let Some(tab) = requested {
            self.select(tab, cx);
        }
    }

    fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if tab == self.tab {
            return;
        }
        self.tab = tab;
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.body = self.profile.clone().map(|profile_id| match self.tab {
            Tab::General | Tab::Downloads | Tab::Integrations | Tab::Advanced => {
                Body::General(cx.new(|cx| GeneralTab::new(self.tab, cx)))
            }
            Tab::Account => Body::Account(cx.new(|cx| AccountTab::new(profile_id, cx))),
            Tab::Compatibility => Body::Compatibility(cx.new(CompatibilityTab::new)),
        });
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let mut items = vec![Tab::General, Tab::Downloads, Tab::Integrations, Tab::Account];
        if crate::launch::in_prefixes() {
            items.push(Tab::Compatibility);
        }
        items.push(Tab::Advanced);
        let ps_offered = Caps::get(cx).is_offered(Feature::PsSection);
        let sidebar = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .flex_none()
            .w(px(200.))
            .children(items.into_iter().enumerate().map(|(index, tab)| {
                let entity = entity.clone();
                sidebar_item(("settings-tab", index), tab.label(), tab == self.tab)
                    .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.select(tab, cx)))
            }))
            .children(ps_offered.then(|| {
                sidebar_item(("settings-tab", 100), "PS".into(), false)
                    .on_click(|_, _, cx| open_ps(cx))
                    .child(div().text_color(rgb(theme::TEXT_MUTED)).child("\u{2197}"))
            }))
            .mt_6();

        let body = match (&AppState::get(cx).session.read(cx).core, &self.body) {
            (Err(err), _) => theme::error(t!("settings.storage_error", error = err)).into_any_element(),
            (Ok(()), Some(Body::General(tab))) => tab.clone().into_any_element(),
            (Ok(()), Some(Body::Account(tab))) => tab.clone().into_any_element(),
            (Ok(()), Some(Body::Compatibility(tab))) => tab.clone().into_any_element(),
            // Shell's onboarding gate means this can't actually happen any
            // more — a profile always exists by the time any screen renders —
            // kept as a fallback rather than an `unreachable!` in case that
            // gate is ever bypassed.
            (Ok(()), None) => theme::empty(t!("common.pick_profile")).into_any_element(),
        };

        theme::page(
            "settings",
            div().flex().flex_col().child(theme::h1(t!("settings.title"))).child(
                div()
                    .flex()
                    .gap_8()
                    .mx_8()
                    .child(sidebar)
                    .child(div().flex_1().min_w_0().child(body)),
            ),
        )
    }
}

fn sidebar_item(id: (&'static str, usize), label: Cow<'static, str>, active: bool) -> Stateful<Div> {
    crate::widgets::nav::tab_stop(div().id(id))
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .py(px(7.))
        .rounded(px(theme::RADIUS_SM))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .when(active, |item| {
            item.text_color(rgb(theme::TEXT_BRIGHT)).bg(rgb(theme::BG_PANEL))
        })
        .when(!active, |item| {
            item.text_color(rgb(theme::TEXT_MUTED))
                .hover(|item| item.text_color(rgb(theme::TEXT_PRIMARY)).bg(rgb(theme::BG_PANEL_2)))
        })
        .child(label)
}

/// A UI-facing name for a storefront — [`Storefront::as_str_name`] is the
/// machine-facing one (a directory/keyring segment), not fit to print.
pub(crate) fn storefront_label(storefront: Storefront) -> Cow<'static, str> {
    match storefront {
        Storefront::Unspecified => t!("settings.account.no_store"),
        Storefront::Steam => "Steam".into(),
        Storefront::EpicGames => "Epic Games".into(),
        Storefront::Gog => "GOG".into(),
        Storefront::AmazonGames => "Amazon Games".into(),
        Storefront::EaApp => "EA App".into(),
        Storefront::UbisoftConnect => "Ubisoft Connect".into(),
        Storefront::BattleNet => "Battle.net".into(),
    }
}

/// `.field-row`: a 520px-max row of controls, 12px above whatever follows.
fn field_row() -> Div {
    div().flex().gap(px(10.)).mb(px(12.)).max_w(px(520.))
}

/// Keeps a button at its content width inside a flex column, where it
/// would otherwise stretch — the webview's buttons were inline-block.
fn inline(element: impl IntoElement) -> Div {
    div().flex().child(element)
}

/// A block of controls that the keyboard focus scrolls into view.
fn section(id: &'static str, content: impl IntoElement) -> impl IntoElement {
    reveal(div().id(id).child(content))
}

/// A tab's own column, so its block-level children stretch the way a
/// webview `div`'s did.
fn column() -> Div {
    div().flex().flex_col()
}
