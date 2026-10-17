//! decanter's own settings, the last section of the main navigation.
//! Three tabs: window behaviour and language (first, since it's where
//! Ajustes opens), the profile's accounts (with where its library lives,
//! which library is active and adding another person), and the default
//! Proton every game runs on unless it pins one of its own (global, not
//! tied to any profile; not on Windows, where games run natively). External
//! games are added from Biblioteca instead (see `crate::views::external`).

mod account;
#[cfg(not(windows))]
mod compatibility;
#[cfg(not(windows))]
mod game_tools;
mod general;

use std::borrow::Cow;

use decanter_core::Storefront;
use gpui::{Context, Div, Entity, IntoElement, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::state::AppState;
use crate::theme;
use account::AccountTab;
#[cfg(not(windows))]
use compatibility::CompatibilityTab;
use general::GeneralTab;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Account,
    #[cfg(not(windows))]
    Compatibility,
}

/// The open tab's own entity. Rebuilt on every tab switch or profile
/// change, so a tab's local state (a half-typed GOG URL, a session check)
/// starts over the way a remounted component's did in the webview build.
enum Body {
    General(Entity<GeneralTab>),
    Account(Entity<AccountTab>),
    #[cfg(not(windows))]
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

    fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if tab == self.tab {
            return;
        }
        self.tab = tab;
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.body = self.profile.clone().map(|profile_id| match self.tab {
            Tab::General => Body::General(cx.new(GeneralTab::new)),
            Tab::Account => Body::Account(cx.new(|cx| AccountTab::new(profile_id, cx))),
            #[cfg(not(windows))]
            Tab::Compatibility => Body::Compatibility(cx.new(CompatibilityTab::new)),
        });
        cx.notify();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let items = [
            (Tab::General, t!("settings.tabs.general")),
            (Tab::Account, t!("settings.tabs.account")),
            #[cfg(not(windows))]
            (Tab::Compatibility, t!("settings.tabs.compatibility")),
        ];
        let tabs = crate::widgets::tabs("settings-tabs", &items, self.tab, move |tab, _, cx| {
            entity.update(cx, |this, cx| this.select(tab, cx));
        })
        .mt_6()
        .mx_8();

        let body = match (&AppState::get(cx).session.read(cx).core, &self.body) {
            (Err(err), _) => theme::error(t!("settings.storage_error", error = err)).into_any_element(),
            (Ok(()), Some(Body::General(tab))) => tab.clone().into_any_element(),
            (Ok(()), Some(Body::Account(tab))) => tab.clone().into_any_element(),
            #[cfg(not(windows))]
            (Ok(()), Some(Body::Compatibility(tab))) => tab.clone().into_any_element(),
            // Shell's onboarding gate means this can't actually happen any
            // more — a profile always exists by the time any screen renders —
            // kept as a fallback rather than an `unreachable!` in case that
            // gate is ever bypassed.
            (Ok(()), None) => theme::empty(t!("common.pick_profile")).into_any_element(),
        };

        theme::page(
            "settings",
            div()
                .flex()
                .flex_col()
                .child(theme::h1(t!("settings.title")))
                .child(tabs)
                .child(body),
        )
    }
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

/// A tab's own column, so its block-level children stretch the way a
/// webview `div`'s did.
fn column() -> Div {
    div().flex().flex_col()
}
