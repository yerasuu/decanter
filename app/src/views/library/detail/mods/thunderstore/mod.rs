//! Adding mods from Thunderstore: the game's community (matched by title, chosen by the user and
//! remembered beside its mods), searching its packages, a package's versions and dependencies, and
//! installing one with what it needs through the same import as any archive
//! (docs/mod-platforms-analysis.md, section 6). The network is `decanter_thunderstore`'s.

mod install;
mod updates;

use std::path::PathBuf;
use std::sync::Arc;

use decanter_thunderstore::{
    Client, Community, CommunityId, GameSettings, Layout, Listing, Package, PackageId, Refresh, VersionNumber,
};
use gpui::{Context, Div, Entity, ObjectFit, SharedString, div, img, prelude::*, px, rgb};
use rust_i18n::t;

use super::ModsTab;
use super::credits::{self, Platform};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::detail::mods::field_row;
use crate::widgets::input::{InputEvent, TextInput};
use crate::widgets::{ButtonKind, Select, button};
pub(super) use install::Install;
pub(super) use updates::UpdateCheck;

/// How many packages a search shows; the rest are counted.
const SHOWN_RESULTS: usize = 30;
const SHOWN_COMMUNITIES: usize = 20;
const SHOWN_VERSIONS: usize = 8;
const SUMMARY_CHARS: usize = 140;

pub(super) struct Browser {
    client: Client,
    cache: PathBuf,
    settings: Option<GameSettings>,
    communities: Option<Arc<[Community]>>,
    suggestions: Vec<usize>,
    is_exact: bool,
    /// The community picker is shown: no choice yet, or the user asked to change it.
    choosing: bool,
    /// The layout the picker saves with the community.
    layout: Layout,
    community_filter: Entity<TextInput>,
    search: Entity<TextInput>,
    listing: Option<Arc<Listing>>,
    results: Vec<PackageId>,
    matched: usize,
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
        let search = cx.new(|cx| TextInput::new(Text::new("mods.thunderstore.search"), cx));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed | InputEvent::Submit) {
                this.run_search(cx);
            }
        })
        .detach();
        let has_settings = settings.is_some();
        tracing::info!(has_settings, "thunderstore browser opened");
        self.thunderstore = Some(Browser {
            client,
            cache,
            layout: settings.as_ref().map_or(Layout::BepInEx, |settings| settings.layout),
            settings,
            communities: None,
            suggestions: Vec::new(),
            is_exact: false,
            choosing: !has_settings,
            community_filter,
            search,
            listing: None,
            results: Vec::new(),
            matched: 0,
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
        if let Err(err) = settings.write(&mods) {
            tracing::error!(%err, "thunderstore game settings could not be saved");
            browser.failure = Some(Failure {
                text: failure_text(&err),
                retry: None,
            });
            cx.notify();
            return;
        }
        let is_same = browser
            .settings
            .as_ref()
            .is_some_and(|known| known.community == settings.community);
        browser.settings = Some(settings);
        browser.choosing = false;
        if is_same && browser.listing.is_some() {
            cx.notify();
        } else {
            browser.listing = None;
            browser.selected = None;
            self.load_listing(Refresh::IfStale, cx);
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
                        if let Some(pending) = browser.pending.take()
                            && listing.get(&pending).is_some()
                        {
                            browser.selected = Some(pending);
                        }
                        browser.listing = Some(Arc::new(listing));
                        this.run_search(cx);
                    }
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

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.thunderstore.as_mut() else {
            return;
        };
        let Some(listing) = browser.listing.as_ref() else {
            return;
        };
        let query = browser.search.read(cx).text().to_owned();
        let (found, matched) = listing.search(&query, SHOWN_RESULTS);
        browser.results = found.into_iter().map(|package| package.id.clone()).collect();
        browser.matched = matched;
        cx.notify();
    }

    fn close_thunderstore(&mut self, cx: &mut Context<Self>) {
        if let Some(browser) = self.thunderstore.take() {
            browser.discard();
        }
        tracing::info!("thunderstore browser closed");
        cx.notify();
    }

    pub(super) fn thunderstore_panel(&self, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let browser = self.thunderstore.as_ref()?;
        let body = if let Some(install) = browser.install.as_ref() {
            install::panel(install, cx)
        } else if let Some(loading) = browser.loading.as_ref() {
            theme::muted(loading)
        } else if browser.choosing {
            self.community_picker(browser, cx)
        } else if let Some(package) = browser
            .selected
            .as_ref()
            .and_then(|id| browser.listing.as_ref()?.get(id))
        {
            detail(package, locked, cx)
        } else {
            results_panel(browser, cx)
        };
        let failure = browser.failure.as_ref().map(|failure| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(theme::error(&failure.text))
                .children(failure.retry.map(|retry| {
                    button("ts-retry", ButtonKind::Secondary, t!("mods.thunderstore.retry"), false).on_click(
                        cx.listener(move |this, _, _, cx| match retry {
                            Retry::Communities => this.load_communities(Refresh::IfStale, cx),
                            Retry::Listing => this.load_listing(Refresh::Now, cx),
                        }),
                    )
                }))
        });
        let working = browser.install.is_some() || browser.loading.is_some();
        Some(
            theme::card()
                .gap_3()
                .px(px(18.))
                .py(px(14.))
                .child(header(browser, working, cx))
                .child(theme::muted(t!("mods.thunderstore.note")))
                .children(failure)
                .child(body)
                .child(credits::footer(Platform::Thunderstore)),
        )
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
        let filter = browser.community_filter.read(cx).text().trim().to_lowercase();
        let shown: Vec<&Community> = if filter.is_empty() {
            browser
                .suggestions
                .iter()
                .filter_map(|&index| communities.get(index))
                .collect()
        } else {
            communities
                .iter()
                .filter(|community| {
                    community.name.to_lowercase().contains(&filter) || community.id.as_str().contains(&filter)
                })
                .take(SHOWN_COMMUNITIES)
                .collect()
        };
        let intro = if !filter.is_empty() {
            t!("mods.thunderstore.community_filtered", count = shown.len())
        } else if browser.suggestions.is_empty() {
            t!("mods.thunderstore.community_none", title = self.target.title)
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
            .children(shown.into_iter().enumerate().map(|(index, community)| {
                let id = community.id.clone();
                div()
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
                    )
            }))
            .child(
                field_row()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(browser.community_filter.clone())),
            )
    }
}

fn results_panel(browser: &Browser, cx: &Context<ModsTab>) -> Div {
    let Some(listing) = browser.listing.as_ref() else {
        return div();
    };
    let rows = browser
        .results
        .iter()
        .filter_map(|id| listing.get(id))
        .enumerate()
        .map(|(index, package)| result_row(index, package, cx));
    let count = if browser.matched > browser.results.len() {
        t!(
            "mods.thunderstore.results_more",
            shown = browser.results.len(),
            count = browser.matched
        )
    } else {
        t!("mods.thunderstore.results", count = browser.matched)
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            field_row()
                .items_center()
                .child(div().flex_1().min_w_0().child(browser.search.clone())),
        )
        .child(theme::muted(count))
        .children(rows)
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
        .child(div().flex_1().child(t!("mods.thunderstore.title")))
        .children(community)
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
        .child(
            button("ts-close", ButtonKind::Secondary, t!("mods.thunderstore.close"), false)
                .on_click(cx.listener(|this, _, _, cx| this.close_thunderstore(cx))),
        )
}

fn icon(url: Option<&str>, size: f32) -> Div {
    let frame = div()
        .flex_none()
        .size(px(size))
        .rounded(px(theme::RADIUS_SM))
        .overflow_hidden()
        .bg(rgb(theme::BG_PANEL));
    match url {
        Some(url) => frame.child(
            img(crate::art::source(url, gpui::size(size, size)))
                .size_full()
                .object_fit(ObjectFit::Cover),
        ),
        None => frame,
    }
}

fn summary(text: &str) -> String {
    if text.chars().count() <= SUMMARY_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(SUMMARY_CHARS).collect();
    cut.push('…');
    cut
}

fn flags(package: &Package) -> Option<Div> {
    let mut flags = Vec::new();
    if package.is_deprecated {
        flags.push(t!("mods.thunderstore.deprecated").into_owned());
    }
    if package.has_nsfw_content {
        flags.push(t!("mods.thunderstore.nsfw").into_owned());
    }
    (!flags.is_empty()).then(|| theme::error(flags.join(" · ")))
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

fn result_row(index: usize, package: &Package, cx: &Context<ModsTab>) -> impl IntoElement + use<> {
    let id = package.id.clone();
    div()
        .id(("ts-result", index))
        .flex()
        .gap_3()
        .py(px(6.))
        .cursor_pointer()
        .border_t_1()
        .border_color(rgb(theme::LINE))
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(browser) = this.thunderstore.as_mut() {
                browser.selected = Some(id.clone());
            }
            cx.notify();
        }))
        .child(icon(package.icon.as_deref(), 40.))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(package.id.display_name())
                        .child(theme::muted(t!(
                            "mods.thunderstore.by",
                            team = package.id.namespace(),
                            version = package.latest.number
                        )))
                        .child(credits::source(Platform::Thunderstore)),
                )
                .child(theme::muted(summary(&package.description)).text_size(px(12.5)))
                .child(theme::muted(stats(package)).text_size(px(12.)))
                .children(flags(package)),
        )
}

fn detail(package: &Package, locked: bool, cx: &Context<ModsTab>) -> Div {
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
        .child(
            div().flex().gap_3().child(icon(package.icon.as_deref(), 64.)).child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(package.id.display_name())
                    .child(theme::muted(t!(
                        "mods.thunderstore.by",
                        team = package.id.namespace(),
                        version = latest.number
                    )))
                    .child(theme::muted(stats(package)).text_size(px(12.)))
                    .children(flags(package)),
            ),
        )
        .child(credits::browsed(
            "ts-credits",
            Platform::Thunderstore,
            decanter_thunderstore::credits(package, latest),
        ))
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
                        t!("mods.thunderstore.open_page"),
                        false,
                    )
                    .on_click(move |_, _, _| {
                        if let Err(err) = crate::desktop::open(&page) {
                            tracing::warn!(%err, "could not open the package's page");
                        }
                    })
                }))
                .child(
                    button("ts-back", ButtonKind::Secondary, t!("mods.thunderstore.back"), false).on_click(
                        cx.listener(|this, _, _, cx| {
                            if let Some(browser) = this.thunderstore.as_mut() {
                                browser.selected = None;
                            }
                            cx.notify();
                        }),
                    ),
                ),
        )
}
