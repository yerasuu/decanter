//! Crediting a mod's authors and its platform, in the browsers and under every installed mod
//! (docs/mod-platforms-attribution.md, section 5): the platform's name as plain text, never a
//! logo nobody has cleared, the author linked to their profile, the mod's page, its licence or
//! permissions as the platform gives them, an adult badge, and a line saying decanter is not the
//! platform's. What an installed mod shows comes from its `mod.toml`, so it holds offline.

use decanter_mods::{ModCredits, ModId, ModOrigin};
use gpui::{Context, Div, SharedString, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::ModsTab;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Platform {
    Thunderstore,
    Modio,
    Nexus,
}

impl Platform {
    pub(super) fn of(origin: &ModOrigin) -> Option<Self> {
        match origin.platform.as_str() {
            decanter_thunderstore::PLATFORM => Some(Self::Thunderstore),
            decanter_modio::PLATFORM => Some(Self::Modio),
            decanter_nexus::PLATFORM => Some(Self::Nexus),
            _ => None,
        }
    }

    /// The platform's own spelling, never translated.
    const fn name(self) -> &'static str {
        match self {
            Self::Thunderstore => "Thunderstore",
            Self::Modio => "mod.io",
            Self::Nexus => "Nexus Mods",
        }
    }
}

fn chip(text: impl Into<SharedString>, color: u32) -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(rgb(theme::LINE))
        .text_size(px(11.5))
        .text_color(rgb(color))
        .child(text.into())
}

/// The platform's name as a label, on every card and detail.
pub(super) fn source(platform: Platform) -> Div {
    chip(platform.name(), theme::TEXT_MUTED)
}

pub(super) fn adult_badge() -> Div {
    chip(t!("mods.credits.adult"), theme::ERROR_TEXT)
}

/// The panel's last line: whose mods these are and that decanter is not the platform's.
pub(super) fn footer(platform: Platform) -> Div {
    theme::muted(t!("mods.credits.footer", platform = platform.name()))
        .text_size(px(12.))
        .pt_2()
        .border_t_1()
        .border_color(rgb(theme::LINE))
}

fn link(id: (SharedString, usize), label: String, url: &str) -> impl IntoElement + use<> {
    let url = url.to_owned();
    div()
        .id(id)
        .cursor_pointer()
        .text_color(rgb(theme::ACCENT_BLUE))
        .child(label)
        .on_click(move |_, _, _| {
            if let Err(err) = crate::desktop::open(&url) {
                tracing::warn!(%err, "could not open a mod credit link");
            }
        })
}

fn permission_label(token: &str) -> String {
    let key = match token {
        "credits" => "mods.credits.permission.credits",
        "original" => "mods.credits.permission.original",
        "redistribute-credit" => "mods.credits.permission.redistribute_credit",
        "port-credit" => "mods.credits.permission.port_credit",
        "patch" => "mods.credits.permission.patch",
        "patch-credit" => "mods.credits.permission.patch_credit",
        "patch-permission" => "mods.credits.permission.patch_permission",
        "repackage" => "mods.credits.permission.repackage",
        "repackage-credit" => "mods.credits.permission.repackage_credit",
        "repackage-permission" => "mods.credits.permission.repackage_permission",
        "resell" => "mods.credits.permission.resell",
        other => return other.to_owned(),
    };
    t!(key).into_owned()
}

fn licence_line(platform: Platform, credits: &ModCredits) -> SharedString {
    if let Some(licence) = credits.licence.as_deref() {
        return t!("mods.credits.licence", text = licence).into_owned().into();
    }
    if !credits.permissions.is_empty() {
        let named = credits
            .permissions
            .iter()
            .map(|token| permission_label(token))
            .collect::<Vec<_>>()
            .join(", ");
        return t!("mods.credits.permissions", platform = platform.name(), list = named)
            .into_owned()
            .into();
    }
    match platform {
        Platform::Thunderstore => t!("mods.credits.licence_thunderstore"),
        Platform::Modio | Platform::Nexus => t!("mods.credits.permissions_page"),
    }
    .into_owned()
    .into()
}

/// The credit lines of a mod a browser shows, whose detail already has a button to its page.
pub(super) fn browsed(scope: &'static str, platform: Platform, mut credits: ModCredits) -> Div {
    credits.page_url = None;
    lines(scope, 0, platform, &credits)
}

/// The credit lines of one mod; `scope` and `index` keep its links' ids apart from another
/// mod's.
fn lines(scope: &'static str, index: usize, platform: Platform, credits: &ModCredits) -> Div {
    let id = |part: &str| (SharedString::from(format!("{scope}-{part}")), index);
    let author = credits.author.as_deref().map(|name| {
        let label = t!("mods.credits.by", author = name).into_owned();
        match credits.author_url.as_deref() {
            Some(url) => link(id("author"), label, url).into_any_element(),
            None => div().child(label).into_any_element(),
        }
    });
    div()
        .flex()
        .flex_col()
        .gap_1()
        .text_size(px(12.5))
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(source(platform))
                .children(credits.is_adult.then(adult_badge))
                .children(author),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_3()
                .children(credits.page_url.as_deref().map(|url| {
                    link(
                        id("page"),
                        t!("mods.credits.view_on", platform = platform.name()).into_owned(),
                        url,
                    )
                }))
                .children(
                    credits
                        .website_url
                        .as_deref()
                        .map(|url| link(id("website"), t!("mods.credits.website").into_owned(), url)),
                ),
        )
        .child(theme::muted(licence_line(platform, credits)))
        .when(platform == Platform::Nexus, |block| {
            block.child(theme::muted(t!("mods.credits.nexus_authorship")))
        })
}

impl ModsTab {
    /// What an installed mod's `mod.toml` credits, or, for one installed before decanter kept
    /// credits, its platform and how to read them.
    pub(super) fn credits_block(index: usize, origin: Option<&ModOrigin>) -> Option<Div> {
        let origin = origin?;
        let platform = Platform::of(origin)?;
        let block = match origin.credits.as_ref() {
            Some(credits) => {
                let read = crate::views::library::fmt::unix_date(credits.fetched_at)
                    .map(|date| theme::muted(t!("mods.credits.read_on", date = date)).text_size(px(12.)));
                lines("mod-credits", index, platform, credits).children(read)
            }
            None => div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(12.5))
                .child(source(platform))
                .child(theme::muted(t!("mods.credits.not_kept"))),
        };
        Some(block.pl(px(29.)))
    }

    /// Writes what an update check read of who made mod `id`, then shows it; a failure only
    /// leaves the old credits, since the check itself succeeded.
    pub(super) fn refresh_credits(&self, id: ModId, credits: ModCredits, cx: &Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let written = tokio::task::spawn_blocking(move || mods.set_credits(&id, credits)).await;
            match written {
                Ok(Ok(())) => {
                    let _ = this.update(cx, |this, cx| this.reload(cx));
                }
                Ok(Err(err)) => tracing::warn!(%err, "mod credits not refreshed"),
                Err(err) => tracing::error!(%err, "refreshing mod credits stopped"),
            }
        })
        .detach();
    }
}
