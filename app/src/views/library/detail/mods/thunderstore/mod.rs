//! Adding mods from Thunderstore: the game's community (matched by title, chosen by the user and
//! remembered beside its mods), searching its packages, a package's versions and dependencies, and
//! installing one with what it needs through the same import as any archive
//! (docs/mod-platforms-analysis.md, section 6). The network is `decanter_thunderstore`'s.

mod install;
mod updates;

use std::path::PathBuf;
use std::sync::Arc;

use decanter_thunderstore::{
    Client, Community, CommunityId, GameSettings, Layout, Listing, Package, PackageId, Query, Refresh, Sort,
    VersionNumber,
};
use gpui::{Context, Div, Entity, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use super::credits::{self, Platform};
use super::search::{self, Provider, ResultItem};
use super::toggles::{self, Installed};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::detail::mods::field_row;
use crate::widgets::input::{InputEvent, TextInput};
use crate::widgets::{ButtonKind, Select, button};
pub(super) use install::Install;
pub(super) use updates::UpdateCheck;

/// How many packages show at first and each *Mostrar más* adds; all matches are in memory.
const PAGE: usize = 50;
const SHOWN_COMMUNITIES: usize = 20;
const SHOWN_VERSIONS: usize = 8;
const SUMMARY_CHARS: usize = 140;

pub(super) struct Browser {
    client: Client,
    cache: PathBuf,
    settings: Option<GameSettings>,
    /// What `thunderstore.toml` holds: a choice is written only once its listing answered.
    saved: Option<GameSettings>,
    communities: Option<Arc<[Community]>>,
    suggestions: Vec<usize>,
    is_exact: bool,
    /// The community picker is shown: no choice yet, or the user asked to change it.
    choosing: bool,
    /// The layout the picker saves with the community.
    layout: Layout,
    community_filter: Entity<TextInput>,
    listing: Option<Arc<Listing>>,
    /// The listing's indices of what matches, in the chosen order.
    results: Vec<usize>,
    shown: usize,
    /// The sort and filters; its words are the search surface's, copied in at each search.
    query: Query,
    selected: Option<PackageId>,
    /// A package to open once the listing is read, from an update check.
    pending: Option<PackageId>,
    loading: Option<Text>,
    failure: Option<Failure>,
    pub(super) install: Option<Install>,
}

struct Failure {
    text: Text,
    retry: Option<Retry>,
}

#[derive(Clone, Copy)]
enum Retry {
    Communities,
    Listing,
}

impl Browser {
    pub(super) const fn is_busy(&self) -> bool {
        self.install.is_some()
    }

    /// Stops its download or import, or drops what it downloaded, as closing it does.
    pub(super) fn discard(self) {
        if let Some(install) = self.install {
            install.discard();
        }
    }
}

/// A failure's line; one Thunderstore may answer differently later says so.
fn failure_text(err: &decanter_thunderstore::Error) -> Text {
    if matches!(err, decanter_thunderstore::Error::Cancelled) {
        Text::new("mods.thunderstore.cancelled")
    } else if err.is_retryable() {
        Text::new("mods.thunderstore.failed_retry").arg("reason", err)
    } else {
        Text::new("mods.thunderstore.failed").arg("reason", err)
    }
}

impl ModsTab {
    pub(super) fn open_thunderstore(&mut self, pending: Option<PackageId>, cx: &mut Context<Self>) {
        self.show_search(Provider::Thunderstore, cx);
        if let Some(browser) = self.thunderstore.as_mut() {
            if pending.is_some() {
                browser.selected.clone_from(&pending);
            }
            return;
        }
        let Some(mods) = self.mods.as_ref() else {
            return;
        };
        let Some(cache) = decanter_thunderstore::cache_dir(mods) else {
            tracing::warn!("no thunderstore cache folder for the game's library");
            return;
        };
        let client = match Client::new() {
            Ok(client) => client,
            Err(err) => {
                tracing::error!(%err, "thunderstore client could not be built");
                self.error = Some(failure_text(&err));
                return;
            }
        };
        let settings = match GameSettings::read(mods) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(%err, "thunderstore game settings unreadable, asking again");
                None
            }
        };
        let community_filter = cx.new(|cx| TextInput::new(Text::new("mods.thunderstore.community_filter"), cx));
        cx.subscribe(&community_filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        let has_settings = settings.is_some();
        tracing::info!(has_settings, "thunderstore browser opened");
        self.thunderstore = Some(Browser {
            client,
            cache,
            layout: settings.as_ref().map_or(Layout::BepInEx, |settings| settings.layout),
            saved: settings.clone(),
            settings,
            communities: None,
            suggestions: Vec::new(),
            is_exact: false,
            choosing: !has_settings,
            community_filter,
            listing: None,
            results: Vec::new(),
            shown: PAGE,
            query: Query::new(),
            selected: None,
            pending,
            loading: None,
            failure: None,
            install: None,
        });
        if has_settings {
            self.load_listing(Refresh::IfStale, cx);
        } else {
            self.load_communities(Refresh::IfStale, cx);
        }
        cx.notify();
    }

    fn load_communities(&mut self, refresh: Refresh, cx: &mut Context<Self>) {
        let title = self.target.title.clone();
        let Some(browser) = self.thunderstore.as_mut() else {
            return;
        };
        browser.loading = Some(Text::new("mods.thunderstore.loading_communities"));
        browser.failure = None;
        let client = browser.client.clone();
        let cache = browser.cache.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let fetched =
                tokio::spawn(async move { decanter_thunderstore::communities(&client, &cache, refresh).await }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.thunderstore.as_mut() else {
                    return;
                };
                browser.loading = None;
                match fetched {
                    Ok(Ok(communities)) => {
                        let suggestions = decanter_thunderstore::suggest(&communities, &title);
                        tracing::info!(
                            communities = communities.len(),
                            candidates = suggestions.candidates.len(),
                            is_exact = suggestions.is_exact,
                            "thunderstore communities matched against the game"
                        );
                        browser.suggestions = suggestions.candidates;
                        browser.is_exact = suggestions.is_exact;
                        browser.communities = Some(Arc::from(communities));
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, retryable = err.is_retryable(), "thunderstore community list failed");
                        browser.failure = Some(Failure {
                            text: failure_text(&err),
                            retry: err.is_retryable().then_some(Retry::Communities),
                        });
                    }
                    Err(err) => {
                        tracing::error!(%err, "thunderstore community list task stopped");
                        browser.failure = Some(Failure {
                            text: Text::verbatim(err),
                            retry: Some(Retry::Communities),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_community(&mut self, community: CommunityId, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.thunderstore.as_mut() else {
            return;
        };
        let settings = GameSettings::new(community, browser.layout);
        let is_same = browser
            .listing
            .as_ref()
            .is_some_and(|listing| *listing.community() == settings.community);
        browser.settings = Some(settings);
        browser.choosing = false;
        browser.failure = None;
        if is_same {
            // Its listing already answered, so the choice is known good.
            Self::ts_save(browser, &mods);
            cx.notify();
        } else {
            browser.listing = None;
            browser.selected = None;
            self.load_listing(Refresh::IfStale, cx);
        }
    }

    /// Writes the choice once its listing answered; until then nothing is kept for the game.
    fn ts_save(browser: &mut Browser, mods: &decanter_mods::GameMods) {
        if browser.settings == browser.saved {
            return;
        }
        let Some(settings) = browser.settings.clone() else {
            return;
        };
        match settings.write(mods) {
            Ok(()) => browser.saved = Some(settings),
            Err(err) => {
                tracing::error!(%err, "thunderstore game settings could not be saved");
                browser.failure = Some(Failure {
                    text: failure_text(&err),
                    retry: None,
                });
            }
        }
    }

    /// The community answers that it does not exist: a saved choice is forgotten and the picker
    /// shows again, inside the section.
    fn ts_community_gone(&mut self, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.thunderstore.as_mut() else {
            return;
        };
        let gone = browser.settings.take().map(|settings| settings.community.to_string());
        tracing::warn!(community = ?gone, saved = browser.saved.is_some(), "thunderstore community not found, choosing again");
        if browser.saved.take().is_some()
            && let Err(err) = GameSettings::forget(&mods)
        {
            tracing::error!(%err, "thunderstore game settings could not be forgotten");
        }
        browser.choosing = true;
        browser.listing = None;
        browser.selected = None;
        browser.failure = Some(Failure {
            text: Text::new("mods.search.mapping_gone").arg("platform", Platform::Thunderstore.name()),
            retry: None,
        });
        if browser.communities.is_none() {
            self.load_communities(Refresh::IfStale, cx);
        }
    }

    fn load_listing(&mut self, refresh: Refresh, cx: &mut Context<Self>) {
        let Some(browser) = self.thunderstore.as_mut() else {
            return;
        };
        let Some(community) = browser.settings.as_ref().map(|settings| settings.community.clone()) else {
            return;
        };
        browser.loading = Some(Text::new("mods.thunderstore.loading_listing"));
        browser.failure = None;
        let client = browser.client.clone();
        let cache = browser.cache.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let fetched =
                tokio::spawn(async move { decanter_thunderstore::listing(&client, &cache, &community, refresh).await })
                    .await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.thunderstore.as_mut() else {
                    return;
                };
                browser.loading = None;
                match fetched {
                    Ok(Ok(listing)) => {
                        if let Some(mods) = this.mods.as_ref() {
                            Self::ts_save(browser, mods);
                        }
                        if let Some(pending) = browser.pending.take()
                            && listing.get(&pending).is_some()
                        {
                            browser.selected = Some(pending);
                        }
                        browser.listing = Some(Arc::new(listing));
                        this.run_search(cx);
                    }
                    Ok(Err(decanter_thunderstore::Error::NotFound { .. })) => this.ts_community_gone(cx),
                    Ok(Err(err)) => {
                        tracing::warn!(%err, retryable = err.is_retryable(), "thunderstore listing failed");
                        browser.failure = Some(Failure {
                            text: failure_text(&err),
                            retry: err.is_retryable().then_some(Retry::Listing),
                        });
                    }
                    Err(err) => {
                        tracing::error!(%err, "thunderstore listing task stopped");
                        browser.failure = Some(Failure {
                            text: Text::verbatim(err),
                            retry: Some(Retry::Listing),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn run_search(&mut self, cx: &mut Context<Self>) {
        let text = self.search_text(cx);
        let Some(browser) = self.thunderstore.as_mut() else {
            return;
        };
        let Some(listing) = browser.listing.as_ref() else {
            return;
        };
        browser.query.text = text;
        browser.results = listing.find(&browser.query);
        browser.shown = PAGE;
        cx.notify();
    }

    /// Changes the sort or a filter, then searches again over the same listing.
    fn ts_refine(&mut self, change: impl FnOnce(&mut Query), cx: &mut Context<Self>) {
        if let Some(browser) = self.thunderstore.as_mut() {
            change(&mut browser.query);
        }
        self.run_search(cx);
    }

    /// The mod of `package` the game has from Thunderstore, if any.
    fn ts_installed(&self, package: &PackageId) -> Option<Installed> {
        let wanted = package.to_string();
        self.installed_mod(
            |origin| origin.platform == decanter_thunderstore::PLATFORM && origin.package == wanted,
            |origin| origin.version.clone(),
        )
    }

    pub(super) fn thunderstore_panel(&self, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let browser = self.thunderstore.as_ref()?;
        let body = if browser.choosing {
            self.community_picker(browser, cx)
        } else {
            let chosen = if let Some(install) = browser.install.as_ref() {
                install::panel(install, self.enables_on_install(), cx)
            } else if let Some(package) = browser
                .selected
                .as_ref()
                .and_then(|id| browser.listing.as_ref()?.get(id))
            {
                detail(package, self.ts_installed(&package.id), locked, cx)
            } else {
                search::nothing_chosen()
            };
            let (controls, list) = self.ts_results(browser, cx);
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(self.query_bar(Provider::Thunderstore, controls, cx))
                .child(search::split(list, chosen))
        };
        let failure = browser.failure.as_ref().map(|failure| {
            let retry = failure.retry.map(|retry| -> search::Retry {
                Box::new(move |this, cx| match retry {
                    Retry::Communities => this.load_communities(Refresh::IfStale, cx),
                    Retry::Listing => this.load_listing(Refresh::Now, cx),
                })
            });
            search::failure_line("ts-retry", &failure.text, retry, None, cx)
        });
        let working = browser.install.is_some() || browser.loading.is_some();
        Some(search::section_frame(
            Platform::Thunderstore,
            header(browser, working, cx),
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(theme::muted(t!("mods.thunderstore.note")).text_size(px(12.5)))
                .children(browser.loading.as_ref().map(theme::muted))
                .children(failure)
                .child(body),
        ))
    }

    fn community_picker(&self, browser: &Browser, cx: &Context<Self>) -> Div {
        let entity = cx.entity();
        let layouts = [
            (
                SharedString::from("bepinex"),
                SharedString::from(t!("mods.thunderstore.layout_bepinex").into_owned()),
            ),
            (
                SharedString::from("plain"),
                SharedString::from(t!("mods.thunderstore.layout_plain").into_owned()),
            ),
        ];
        let selected = if browser.layout == Layout::Plain {
            "plain"
        } else {
            "bepinex"
        };
        let layout = Select::new("ts-layout", layouts, selected).on_change(move |chosen, _, cx| {
            let layout = if chosen.as_ref() == "plain" {
                Layout::Plain
            } else {
                Layout::BepInEx
            };
            entity.update(cx, |this, cx| {
                if let Some(browser) = this.thunderstore.as_mut() {
                    browser.layout = layout;
                }
                cx.notify();
            });
        });
        let Some(communities) = browser.communities.as_ref() else {
            return div().child(
                button(
                    "ts-load-communities",
                    ButtonKind::Secondary,
                    t!("mods.thunderstore.load_communities"),
                    false,
                )
                .on_click(cx.listener(|this, _, _, cx| this.load_communities(Refresh::IfStale, cx))),
            );
        };
        let title = &self.target.title;
        let filter = browser.community_filter.read(cx).text().trim().to_lowercase();
        // A hand search shows what the user typed, the ones that look like the game first and the
        // rest under their own heading, never mixed with the suggestions.
        let (shown, others): (Vec<&Community>, Vec<&Community>) = if filter.is_empty() {
            (
                browser
                    .suggestions
                    .iter()
                    .filter_map(|&index| communities.get(index))
                    .collect(),
                Vec::new(),
            )
        } else {
            communities
                .iter()
                .filter(|community| {
                    community.name.to_lowercase().contains(&filter) || community.id.as_str().contains(&filter)
                })
                .take(SHOWN_COMMUNITIES)
                .partition(|community| {
                    decanter_mods::best_likeness(title, [community.name.as_str(), community.id.as_str()]).is_some()
                })
        };
        let intro = if !filter.is_empty() {
            t!(
                "mods.thunderstore.community_filtered",
                count = shown.len().saturating_add(others.len())
            )
        } else if browser.suggestions.is_empty() {
            t!("mods.search.no_match", platform = Platform::Thunderstore.name())
        } else if browser.is_exact {
            t!("mods.thunderstore.community_exact", title = self.target.title)
        } else {
            t!("mods.thunderstore.community_ambiguous", title = self.target.title)
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                field_row()
                    .items_center()
                    .child(div().w(px(110.)).child(theme::muted(t!("mods.thunderstore.layout"))))
                    .child(layout),
            )
            .child(theme::muted(t!("mods.thunderstore.layout_note")))
            .child(theme::muted(intro))
            .children(shown.iter().chain(&others).enumerate().map(|(index, community)| {
                let heading = (index == shown.len()).then(search::other_results);
                let id = community.id.clone();
                let row = div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().child(community.name.clone()))
                    .child(theme::muted(community.id.to_string()))
                    .child(
                        button(
                            ("ts-community", index),
                            ButtonKind::Secondary,
                            t!("mods.thunderstore.use"),
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_community(id.clone(), cx))),
                    );
                div().flex().flex_col().children(heading).child(row)
            }))
            .child(
                field_row()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(browser.community_filter.clone())),
            )
    }

    /// The sort and filter controls, then the results under them.
    fn ts_results(&self, browser: &Browser, cx: &Context<Self>) -> (Div, Div) {
        let Some(listing) = browser.listing.as_ref() else {
            return (div(), div());
        };
        let sorts = vec![
            (Sort::Relevance, t!("mods.search.sort_relevance").into_owned().into()),
            (Sort::Downloads, t!("mods.search.sort_downloads").into_owned().into()),
            (Sort::Rating, t!("mods.search.sort_rating").into_owned().into()),
            (Sort::Updated, t!("mods.search.sort_updated").into_owned().into()),
            (Sort::Name, t!("mods.search.sort_name").into_owned().into()),
        ];
        let sort = search::choice(
            "ts-sort",
            t!("mods.search.sort").into_owned().into(),
            sorts,
            browser.query.sort,
            |this, sort, cx| this.ts_refine(|query| query.sort = sort, cx),
            cx,
        );
        let categories = listing.categories();
        let category = (!categories.is_empty()).then(|| {
            let options = core::iter::once((None, SharedString::from(t!("mods.search.any").into_owned())))
                .chain(
                    categories
                        .iter()
                        .enumerate()
                        .map(|(index, name)| (Some(index), SharedString::from(name.clone()))),
                )
                .collect();
            let selected = browser
                .query
                .category
                .as_ref()
                .and_then(|wanted| categories.iter().position(|name| name == wanted));
            search::choice(
                "ts-category",
                t!("mods.search.category").into_owned().into(),
                options,
                selected,
                |this, chosen: Option<usize>, cx| {
                    let name = chosen.and_then(|index| {
                        this.thunderstore
                            .as_ref()?
                            .listing
                            .as_ref()?
                            .categories()
                            .get(index)
                            .cloned()
                    });
                    this.ts_refine(|query| query.category = name, cx);
                },
                cx,
            )
        });
        let controls = search::controls()
            .child(sort)
            .children(category)
            .child(search::flag(
                "ts-hide-deprecated",
                t!("mods.search.hide_deprecated").into_owned().into(),
                browser.query.hide_deprecated,
                |this, on, cx| this.ts_refine(|query| query.hide_deprecated = on, cx),
                cx,
            ))
            .child(search::flag(
                "ts-hide-nsfw",
                t!("mods.search.hide_nsfw").into_owned().into(),
                browser.query.hide_nsfw,
                |this, on, cx| this.ts_refine(|query| query.hide_nsfw = on, cx),
                cx,
            ));
        let shown = browser.results.len().min(browser.shown);
        let rows = browser
            .results
            .iter()
            .take(shown)
            .filter_map(|&index| listing.at(index))
            .enumerate()
            .map(|(row, package)| self.ts_result_row(row, package, cx));
        let has_more = browser.results.len() > shown;
        let list = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(search::count_line(shown, Some(browser.results.len() as u64)))
            .children(rows)
            .children(has_more.then(|| {
                search::load_more(
                    "ts-more",
                    |this, cx| {
                        if let Some(browser) = this.thunderstore.as_mut() {
                            browser.shown = browser.shown.saturating_add(PAGE);
                        }
                        cx.notify();
                    },
                    cx,
                )
            }));
        (controls, list)
    }

    fn ts_result_row(&self, row: usize, package: &Package, cx: &Context<Self>) -> impl IntoElement + use<> {
        let id = package.id.clone();
        let name = package.id.display_name();
        search::result_row(
            ("ts-result", row),
            ResultItem {
                picture: package.icon.as_deref(),
                name: &name,
                by: Some(
                    t!(
                        "mods.thunderstore.by",
                        team = package.id.namespace(),
                        version = package.latest.number
                    )
                    .into_owned(),
                ),
                summary: search::summary(&package.description, SUMMARY_CHARS),
                stats: stats(package),
                flags: flags(package),
                platform: Platform::Thunderstore,
                is_adult: false,
                installed: self.ts_installed(&package.id),
            },
            move |this, cx| {
                if let Some(browser) = this.thunderstore.as_mut() {
                    browser.selected = Some(id.clone());
                }
                cx.notify();
            },
            cx,
        )
    }
}

fn header(browser: &Browser, working: bool, cx: &Context<ModsTab>) -> Div {
    let community = browser.settings.as_ref().map(|settings| {
        let name = browser
            .communities
            .as_ref()
            .and_then(|all| all.iter().find(|known| known.id == settings.community))
            .map_or_else(|| settings.community.to_string(), |known| known.name.clone());
        let layout = if settings.layout == Layout::Plain {
            t!("mods.thunderstore.layout_plain")
        } else {
            t!("mods.thunderstore.layout_bepinex")
        };
        theme::muted(t!(
            "mods.thunderstore.community",
            name = name,
            layout = layout,
            count = browser.listing.as_ref().map_or(0, |listing| listing.len())
        ))
    });
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(div().flex_1().children(community))
        .children((browser.settings.is_some() && !browser.choosing).then(|| {
            button(
                "ts-change",
                ButtonKind::Secondary,
                t!("mods.thunderstore.change"),
                working,
            )
            .when(!working, |b| {
                b.on_click(cx.listener(|this, _, _, cx| {
                    if let Some(browser) = this.thunderstore.as_mut() {
                        browser.choosing = true;
                    }
                    let needs_list = this
                        .thunderstore
                        .as_ref()
                        .is_some_and(|browser| browser.communities.is_none());
                    if needs_list {
                        this.load_communities(Refresh::IfStale, cx);
                    }
                    cx.notify();
                }))
            })
        }))
        .children(browser.listing.is_some().then(|| {
            button(
                "ts-refresh",
                ButtonKind::Secondary,
                t!("mods.thunderstore.refresh"),
                working,
            )
            .when(!working, |b| {
                b.on_click(cx.listener(|this, _, _, cx| this.load_listing(Refresh::Now, cx)))
            })
        }))
}

fn flags(package: &Package) -> Vec<String> {
    let mut flags = Vec::new();
    if package.is_deprecated {
        flags.push(t!("mods.thunderstore.deprecated").into_owned());
    }
    if package.has_nsfw_content {
        flags.push(t!("mods.thunderstore.nsfw").into_owned());
    }
    flags
}

fn stats(package: &Package) -> String {
    t!(
        "mods.thunderstore.stats",
        downloads = package.downloads,
        rating = package.rating,
        updated = package.updated.as_deref().unwrap_or("-")
    )
    .into_owned()
}

fn detail(package: &Package, installed: Option<Installed>, locked: bool, cx: &Context<ModsTab>) -> Div {
    let latest = &package.latest;
    let id = package.id.clone();
    let number: VersionNumber = latest.number;
    let page = package.page.clone();
    let dependencies = if latest.dependencies.is_empty() {
        theme::muted(t!("mods.thunderstore.no_dependencies"))
    } else {
        div()
            .flex()
            .flex_col()
            .child(theme::muted(t!(
                "mods.thunderstore.dependencies",
                count = latest.dependencies.len()
            )))
            .children(
                latest
                    .dependencies
                    .iter()
                    .map(|dependency| theme::mono().pl_2().text_size(px(12.5)).child(dependency.to_string())),
            )
    };
    let size = latest.file_size.map_or_else(
        || t!("mods.thunderstore.size_unknown").into_owned(),
        crate::format::bytes,
    );
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(search::detail_head(
            package.icon.as_deref(),
            &package.id.display_name(),
            [
                theme::muted(t!(
                    "mods.thunderstore.by",
                    team = package.id.namespace(),
                    version = latest.number
                )),
                theme::muted(stats(package)).text_size(px(12.)),
            ]
            .into_iter()
            .chain({
                let flags = flags(package);
                (!flags.is_empty()).then(|| theme::error(flags.join(" · ")))
            }),
        ))
        .child(credits::browsed(
            "ts-credits",
            Platform::Thunderstore,
            decanter_thunderstore::credits(package, latest),
        ))
        .children(installed.map(|installed| toggles::installed_controls(0, &installed, cx)))
        .child(div().child(package.description.clone()))
        .child(theme::muted(t!("mods.thunderstore.download_size", size = size)))
        .child(dependencies)
        .child(theme::muted(t!("mods.thunderstore.versions")))
        .children(package.versions().take(SHOWN_VERSIONS).map(|version| {
            theme::muted(t!(
                "mods.thunderstore.version_line",
                version = version.number,
                date = version.created.as_deref().unwrap_or("-"),
                downloads = version.downloads
            ))
            .pl_2()
            .text_size(px(12.5))
        }))
        .child(
            div()
                .flex()
                .gap(px(10.))
                .child(
                    button(
                        "ts-install",
                        ButtonKind::Primary,
                        t!("mods.thunderstore.install"),
                        locked,
                    )
                    .when(!locked, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.install_package(id.clone(), number, cx)))
                    }),
                )
                .children(page.map(|page| {
                    button(
                        "ts-page",
                        ButtonKind::Secondary,
                        t!("mods.credits.view_on", platform = Platform::Thunderstore.name()),
                        false,
                    )
                    .on_click(move |_, _, _| {
                        if let Err(err) = crate::desktop::open(&page) {
                            tracing::warn!(%err, "could not open the package's page");
                        }
                    })
                })),
        )
}
