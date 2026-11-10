//! The Mods tab's sections: *Instalados*, then one whole section per provider, each a master-detail
//! view over the tab's full width built from the same parts (top bar with the mapped game, search
//! bar with sorts and filters, results on the left, the chosen mod on the right). Never one merged
//! list (decision #1635, design section 2.5): the providers answer at different costs
//! (Thunderstore from a local listing, mod.io at 1.5 s a request, Nexus out of an hourly allowance),
//! sort by numbers that do not compare, and install under different rules.
//!
//! A provider's browser is opened the first time its section is shown and stays, with what it
//! read and the mod chosen, while the user visits other sections; the tab dropping it stops its
//! downloads and imports. The query box is one for every section, so its words follow the user.

mod parts;

use decanter_mods::ModOrigin;
use gpui::{App, Context, Div, Entity, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use super::credits::Platform;
use crate::i18n::Text;
use crate::theme;
use crate::widgets::input::{InputEvent, TextInput};
use crate::widgets::{ButtonKind, button};
pub(super) use parts::{
    ResultItem, Retry, choice, controls, count_line, detail_head, failure_line, flag, load_more, nothing_chosen,
    other_results, picture, quota_line, result_row, section_frame, segments, split, summary,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Provider {
    Thunderstore,
    Modio,
    Nexus,
}

impl Provider {
    pub(super) const fn platform(self) -> Platform {
        match self {
            Self::Thunderstore => Platform::Thunderstore,
            Self::Modio => Platform::Modio,
            Self::Nexus => Platform::Nexus,
        }
    }

    /// Whether typing searches: Thunderstore's listing and Nexus' lists are already in memory,
    /// while every mod.io search is a request and goes only on Enter or *Buscar*.
    const fn is_live(self) -> bool {
        !matches!(self, Self::Modio)
    }
}

/// What the Mods tab shows; a new section (packs) is one more variant here and in
/// [`ModsTab::section_nav`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Installed,
    Provider(Provider),
}

pub(super) struct Surface {
    section: Section,
    query: Entity<TextInput>,
}

impl Surface {
    pub(super) fn new(cx: &mut Context<ModsTab>) -> Self {
        let query = cx.new(|cx| TextInput::new(Text::new("mods.search.query"), cx));
        cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            let Section::Provider(provider) = this.search.section else {
                return;
            };
            match event {
                InputEvent::Changed if provider.is_live() => this.search_provider(provider, cx),
                InputEvent::Submit => this.search_provider(provider, cx),
                InputEvent::Changed | InputEvent::Cancel => {}
            }
        })
        .detach();
        Self {
            section: Section::Installed,
            query,
        }
    }

    pub(super) const fn section(&self) -> Section {
        self.section
    }
}

impl ModsTab {
    /// The words in the shared query box.
    pub(super) fn search_text(&self, cx: &App) -> String {
        self.search.query.read(cx).text().trim().to_owned()
    }

    /// The origin of the last mod in the load order `matches` accepts, for a result's *installed* mark.
    pub(super) fn installed_origin(&self, matches: impl Fn(&ModOrigin) -> bool) -> Option<&ModOrigin> {
        self.loaded
            .as_ref()?
            .rows
            .iter()
            .filter_map(|row| row.manifest.origin.as_ref())
            .filter(|origin| matches(origin))
            .last()
    }

    /// Shows `provider`'s section without opening its browser; every way into a browser goes
    /// through here, so that its section is the one shown.
    pub(super) fn show_search(&mut self, provider: Provider, cx: &mut Context<Self>) {
        self.search.section = Section::Provider(provider);
        cx.notify();
    }

    pub(super) fn show_installed(&mut self, cx: &mut Context<Self>) {
        self.search.section = Section::Installed;
        cx.notify();
    }

    fn select_section(&mut self, section: Section, cx: &mut Context<Self>) {
        let Section::Provider(provider) = section else {
            self.show_installed(cx);
            return;
        };
        self.show_search(provider, cx);
        match provider {
            Provider::Thunderstore if self.thunderstore.is_none() => self.open_thunderstore(None, cx),
            Provider::Modio if self.modio.is_none() => self.open_modio(None, cx),
            Provider::Nexus if self.nexus.is_none() => self.open_nexus(None, cx),
            Provider::Thunderstore => self.run_search(cx),
            Provider::Modio | Provider::Nexus => {}
        }
    }

    fn search_provider(&mut self, provider: Provider, cx: &mut Context<Self>) {
        match provider {
            Provider::Thunderstore => self.run_search(cx),
            Provider::Modio => self.modio_search(0, cx),
            Provider::Nexus => cx.notify(),
        }
    }

    /// Ajustes' switch turned Nexus off: its browser went with it, so the tab leaves its section.
    pub(super) fn follow_nexus_switch(&mut self) {
        if !self.nexus_experimental && self.search.section == Section::Provider(Provider::Nexus) {
            self.search.section = Section::Installed;
        }
    }

    /// The tab's sub-navigation, Nexus Mods only while its experimental switch is on.
    pub(super) fn section_nav(&self, cx: &Context<Self>) -> Div {
        let installed = self.loaded.as_ref().map_or(0, |loaded| loaded.rows.len());
        let mut sections = vec![
            (
                Section::Installed,
                SharedString::from(t!("mods.section.installed", count = installed).into_owned()),
            ),
            (
                Section::Provider(Provider::Thunderstore),
                SharedString::from(Platform::Thunderstore.name()),
            ),
            (
                Section::Provider(Provider::Modio),
                SharedString::from(Platform::Modio.name()),
            ),
        ];
        if self.nexus_experimental {
            sections.push((
                Section::Provider(Provider::Nexus),
                SharedString::from(Platform::Nexus.name()),
            ));
        }
        segments("mod-section", &sections, self.search.section, Self::select_section, cx)
    }

    /// The shown provider's whole section, `None` on *Instalados*.
    pub(super) fn provider_section(&self, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let Section::Provider(provider) = self.search.section else {
            return None;
        };
        let section = match provider {
            Provider::Thunderstore => self.thunderstore_panel(locked, cx),
            Provider::Modio => self.modio_panel(locked, cx),
            Provider::Nexus => self.nexus_panel(locked, cx),
        };
        Some(section.unwrap_or_else(|| reopen(provider, cx)))
    }

    /// The query box over a provider's results, with its submit button where a search costs a
    /// request, then `controls`.
    pub(super) fn query_bar(&self, provider: Provider, controls: Div, cx: &Context<Self>) -> Div {
        let note = if provider.is_live() {
            t!("mods.search.live_note")
        } else {
            t!("mods.search.submit_note")
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .max_w(px(640.))
                    .child(div().flex_1().min_w_0().child(self.search.query.clone()))
                    .children((!provider.is_live()).then(|| {
                        button(
                            "mod-search-submit",
                            ButtonKind::Secondary,
                            t!("mods.search.submit"),
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.search_provider(provider, cx)))
                    })),
            )
            .child(controls)
            .child(theme::muted(note).text_size(px(12.)))
    }
}

/// A provider whose browser was closed (mod.io's terms declined): one press opens it again.
fn reopen(provider: Provider, cx: &Context<ModsTab>) -> Div {
    let name = provider.platform().name();
    section_frame(
        provider.platform(),
        div().child(name),
        div().flex().child(
            button(
                "mod-search-reopen",
                ButtonKind::Secondary,
                t!("mods.search.reopen", platform = name),
                false,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select_section(Section::Provider(provider), cx))),
        ),
    )
}
