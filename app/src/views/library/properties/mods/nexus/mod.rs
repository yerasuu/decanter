//! Adding mods from Nexus Mods with the user's own key: the account it belongs to, the game's
//! Nexus domain (matched by title, chosen by the user and remembered beside its mods), the latest,
//! trending and updated mods or one opened by number or page address, a mod's files, and for a
//! Premium account downloading one into the same import as any archive (docs/mods.md). A free
//! account gets the page and the file list and adds the file it downloads as any archive, since
//! Nexus gives free accounts no download link through the API. The network is `decanter_nexus`'s.

mod install;
mod updates;

use std::path::PathBuf;
use std::sync::Arc;

use decanter_nexus::{
    Account, Category, Client, Feed, FileList, Game, GameDomain, GameSettings, ModInfo, NexusFile, NexusModId, Refresh,
};
use gpui::{Context, Div, Entity, ObjectFit, SharedString, div, img, prelude::*, px, rgb};
use rust_i18n::t;

use super::ModsTab;
use crate::i18n::Text;
use crate::nexus::{account_line, failure_text};
use crate::theme;
use crate::views::library::properties::field_row;
use crate::widgets::input::{InputEvent, TextInput};
use crate::widgets::{ButtonKind, Select, button};
pub(super) use install::Install;
pub(super) use updates::UpdateCheck;

const SHOWN_GAMES: usize = 20;
const SUMMARY_CHARS: usize = 160;

pub(super) struct Browser {
    client: Option<Client>,
    account: Option<Account>,
    cache: PathBuf,
    settings: Option<GameSettings>,
    games: Option<Arc<[Game]>>,
    suggestions: Vec<usize>,
    is_exact: bool,
    /// The game picker is shown: no choice yet, or the user asked to change it.
    choosing: bool,
    game_filter: Entity<TextInput>,
    filter: Entity<TextInput>,
    reference: Entity<TextInput>,
    feed: Feed,
    mods: Vec<ModInfo>,
    selected: Option<Selected>,
    /// A mod to open once the account is known, from an update check.
    pending: Option<NexusModId>,
    loading: Option<Text>,
    failure: Option<Failure>,
    pub(super) install: Option<Install>,
}

struct Selected {
    info: ModInfo,
    files: FileList,
}

struct Failure {
    text: Text,
    retry: Option<Retry>,
}

#[derive(Clone, Copy)]
enum Retry {
    Account,
    Games,
    Feed,
}

impl Browser {
    pub(super) const fn is_busy(&self) -> bool {
        self.install.is_some()
    }

    fn domain(&self) -> Option<&GameDomain> {
        self.settings.as_ref().map(|settings| &settings.domain)
    }

    const fn is_premium(&self) -> bool {
        matches!(&self.account, Some(account) if account.is_premium)
    }
}

impl ModsTab {
    pub(super) fn open_nexus(&mut self, pending: Option<NexusModId>, cx: &mut Context<Self>) {
        if self.nexus.is_some() {
            if let Some(id) = pending {
                self.open_mod(id, cx);
            }
            return;
        }
        let Some(mods) = self.mods.as_ref() else {
            return;
        };
        let Some(cache) = decanter_nexus::cache_dir(mods) else {
            tracing::warn!("no nexus cache folder for the game's library");
            return;
        };
        let settings = match GameSettings::read(mods) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(%err, "nexus game settings unreadable, asking again");
                None
            }
        };
        let game_filter = cx.new(|cx| TextInput::new(Text::new("mods.nexus.game_filter"), cx));
        cx.subscribe(&game_filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        let filter = cx.new(|cx| TextInput::new(Text::new("mods.nexus.filter"), cx));
        cx.subscribe(&filter, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Changed) {
                cx.notify();
            }
        })
        .detach();
        let reference = cx.new(|cx| TextInput::new(Text::new("mods.nexus.reference"), cx));
        cx.subscribe(&reference, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit) {
                this.open_reference(cx);
            }
        })
        .detach();
        tracing::info!(has_settings = settings.is_some(), "nexus browser opened");
        self.nexus = Some(Browser {
            client: None,
            account: None,
            cache,
            choosing: settings.is_none(),
            settings,
            games: None,
            suggestions: Vec::new(),
            is_exact: false,
            game_filter,
            filter,
            reference,
            feed: Feed::Latest,
            mods: Vec::new(),
            selected: None,
            pending,
            loading: None,
            failure: None,
            install: None,
        });
        self.load_account(cx);
    }

    /// Reads the key and asks whose it is; what to show next waits for the answer, since every
    /// other request needs a key Nexus takes.
    fn load_account(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.nexus.as_mut() else {
            return;
        };
        browser.loading = Some(Text::new("mods.nexus.loading_account"));
        browser.failure = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let checked = tokio::spawn(async {
                let client = crate::nexus::client().await?;
                let account = decanter_nexus::validate(&client).await?;
                Ok::<_, decanter_nexus::Error>((client, account))
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.nexus.as_mut() else {
                    return;
                };
                browser.loading = None;
                match checked {
                    Ok(Ok((client, account))) => {
                        browser.client = Some(client);
                        browser.account = Some(account);
                        if browser.choosing {
                            this.load_games(Refresh::IfStale, cx);
                        } else if let Some(id) = browser.pending.take() {
                            this.open_mod(id, cx);
                        } else {
                            this.load_feed(cx);
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, retryable = err.is_retryable(), "nexus account not checked");
                        browser.failure = Some(Failure {
                            text: failure_text(&err),
                            retry: err.is_retryable().then_some(Retry::Account),
                        });
                    }
                    Err(err) => {
                        tracing::error!(%err, "nexus account check stopped");
                        browser.failure = Some(Failure {
                            text: Text::verbatim(err),
                            retry: Some(Retry::Account),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_games(&mut self, refresh: Refresh, cx: &mut Context<Self>) {
        let title = self.target.title.clone();
        let Some(browser) = self.nexus.as_mut() else {
            return;
        };
        let Some(client) = browser.client.clone() else {
            return;
        };
        browser.loading = Some(Text::new("mods.nexus.loading_games"));
        browser.failure = None;
        let cache = browser.cache.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let fetched = tokio::spawn(async move { decanter_nexus::games(&client, &cache, refresh).await }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.nexus.as_mut() else {
                    return;
                };
                browser.loading = None;
                match fetched {
                    Ok(Ok(games)) => {
                        let suggestions = decanter_nexus::suggest(&games, &title);
                        tracing::info!(
                            games = games.len(),
                            candidates = suggestions.candidates.len(),
                            is_exact = suggestions.is_exact,
                            "nexus games matched against the game"
                        );
                        browser.suggestions = suggestions.candidates;
                        browser.is_exact = suggestions.is_exact;
                        browser.games = Some(Arc::from(games));
                    }
                    Ok(Err(err)) => {
                        tracing::warn!(%err, retryable = err.is_retryable(), "nexus game list failed");
                        browser.failure = Some(Failure {
                            text: failure_text(&err),
                            retry: err.is_retryable().then_some(Retry::Games),
                        });
                    }
                    Err(err) => {
                        tracing::error!(%err, "nexus game list task stopped");
                        browser.failure = Some(Failure {
                            text: Text::verbatim(err),
                            retry: Some(Retry::Games),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_game(&mut self, domain: GameDomain, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.nexus.as_mut() else {
            return;
        };
        let settings = GameSettings::new(domain);
        if let Err(err) = settings.write(&mods) {
            tracing::error!(%err, "nexus game settings could not be saved");
            browser.failure = Some(Failure {
                text: failure_text(&err),
                retry: None,
            });
            cx.notify();
            return;
        }
        let is_same = browser.domain() == Some(&settings.domain);
        browser.settings = Some(settings);
        browser.choosing = false;
        if is_same && !browser.mods.is_empty() {
            cx.notify();
        } else {
            browser.mods.clear();
            browser.selected = None;
            self.load_feed(cx);
        }
    }

    fn load_feed(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.nexus.as_mut() else {
            return;
        };
        let (Some(client), Some(domain)) = (browser.client.clone(), browser.domain().cloned()) else {
            return;
        };
        let which = browser.feed;
        browser.loading = Some(Text::new("mods.nexus.loading_mods"));
        browser.failure = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let fetched = tokio::spawn(async move { decanter_nexus::feed(&client, &domain, which).await }).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.nexus.as_mut() else {
                    return;
                };
                browser.loading = None;
                match fetched {
                    Ok(Ok(found)) => browser.mods = found,
                    Ok(Err(err)) => {
                        tracing::warn!(%err, retryable = err.is_retryable(), "nexus mod list failed");
                        browser.failure = Some(Failure {
                            text: failure_text(&err),
                            retry: err.is_retryable().then_some(Retry::Feed),
                        });
                    }
                    Err(err) => {
                        tracing::error!(%err, "nexus mod list task stopped");
                        browser.failure = Some(Failure {
                            text: Text::verbatim(err),
                            retry: Some(Retry::Feed),
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_reference(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.nexus.as_mut() else {
            return;
        };
        let Some(domain) = browser.domain() else {
            return;
        };
        let pasted = browser.reference.read(cx).text().to_owned();
        match decanter_nexus::parse_mod_reference(&pasted, domain) {
            Ok(id) => self.open_mod(id, cx),
            Err(err) => {
                tracing::info!(%err, "nexus mod reference refused");
                browser.failure = Some(Failure {
                    text: Text::new("mods.nexus.bad_reference").arg("domain", domain),
                    retry: None,
                });
                cx.notify();
            }
        }
    }

    /// The mod's details and its files, two requests, both asked because the user opened it.
    fn open_mod(&mut self, id: NexusModId, cx: &mut Context<Self>) {
        let Some(browser) = self.nexus.as_mut() else {
            return;
        };
        let (Some(client), Some(domain)) = (browser.client.clone(), browser.domain().cloned()) else {
            browser.pending = Some(id);
            return;
        };
        browser.loading = Some(Text::new("mods.nexus.loading_mod"));
        browser.failure = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let fetched = tokio::spawn(async move {
                let info = decanter_nexus::mod_info(&client, &domain, id).await?;
                let files = decanter_nexus::files(&client, &domain, id).await?;
                Ok::<_, decanter_nexus::Error>(Selected { info, files })
            })
            .await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.nexus.as_mut() else {
                    return;
                };
                browser.loading = None;
                match fetched {
                    Ok(Ok(selected)) => browser.selected = Some(selected),
                    Ok(Err(err)) => {
                        tracing::warn!(%err, mod_id = %id, "nexus mod not read");
                        browser.failure = Some(Failure {
                            text: failure_text(&err),
                            retry: None,
                        });
                    }
                    Err(err) => {
                        tracing::error!(%err, "nexus mod task stopped");
                        browser.failure = Some(Failure {
                            text: Text::verbatim(err),
                            retry: None,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn close_nexus(&mut self, cx: &mut Context<Self>) {
        if let Some(browser) = self.nexus.take()
            && let Some(install) = browser.install
        {
            install.discard();
        }
        tracing::info!("nexus browser closed");
        cx.notify();
    }

    pub(super) fn nexus_panel(&self, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let browser = self.nexus.as_ref()?;
        let body = if let Some(install) = browser.install.as_ref() {
            install::panel(install, cx)
        } else if let Some(loading) = browser.loading.as_ref() {
            theme::muted(loading)
        } else if browser.account.is_none() {
            no_account(browser)
        } else if browser.choosing {
            self.game_picker(browser, cx)
        } else if let Some(selected) = browser.selected.as_ref() {
            detail(browser, selected, locked, cx)
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
                    button("nx-retry", ButtonKind::Secondary, t!("mods.nexus.retry"), false).on_click(cx.listener(
                        move |this, _, _, cx| match retry {
                            Retry::Account => this.load_account(cx),
                            Retry::Games => this.load_games(Refresh::Now, cx),
                            Retry::Feed => this.load_feed(cx),
                        },
                    ))
                }))
        });
        let working = browser.install.is_some() || browser.loading.is_some();
        Some(
            theme::card()
                .gap_3()
                .px(px(18.))
                .py(px(14.))
                .child(header(browser, working, cx))
                .child(theme::muted(t!("mods.nexus.note")))
                .children(
                    browser
                        .account
                        .as_ref()
                        .map(|account| theme::muted(account_line(account))),
                )
                .children(remaining_line())
                .children(failure)
                .child(body),
        )
    }

    fn game_picker(&self, browser: &Browser, cx: &Context<Self>) -> Div {
        let Some(games) = browser.games.as_ref() else {
            return div().child(
                button(
                    "nx-load-games",
                    ButtonKind::Secondary,
                    t!("mods.nexus.load_games"),
                    false,
                )
                .on_click(cx.listener(|this, _, _, cx| this.load_games(Refresh::IfStale, cx))),
            );
        };
        let filter = browser.game_filter.read(cx).text().trim().to_lowercase();
        let shown: Vec<&Game> = if filter.is_empty() {
            browser
                .suggestions
                .iter()
                .filter_map(|&index| games.get(index))
                .collect()
        } else {
            games
                .iter()
                .filter(|game| game.name.to_lowercase().contains(&filter) || game.domain.as_str().contains(&filter))
                .take(SHOWN_GAMES)
                .collect()
        };
        let intro = if !filter.is_empty() {
            t!("mods.nexus.game_filtered", count = shown.len())
        } else if browser.suggestions.is_empty() {
            t!("mods.nexus.game_none", title = self.target.title)
        } else if browser.is_exact {
            t!("mods.nexus.game_exact", title = self.target.title)
        } else {
            t!("mods.nexus.game_ambiguous", title = self.target.title)
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(theme::muted(intro))
            .children(shown.into_iter().enumerate().map(|(index, game)| {
                let domain = game.domain.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().child(game.name.clone()))
                    .child(theme::muted(game.domain.to_string()))
                    .child(
                        button(("nx-game", index), ButtonKind::Secondary, t!("mods.nexus.use"), false)
                            .on_click(cx.listener(move |this, _, _, cx| this.choose_game(domain.clone(), cx))),
                    )
            }))
            .child(
                field_row()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(browser.game_filter.clone())),
            )
    }
}

fn no_account(browser: &Browser) -> Div {
    let is_missing_key = browser
        .failure
        .as_ref()
        .is_some_and(|failure| failure.text.is_or_wraps("mods.nexus.no_key"));
    div().flex().flex_col().gap_2().children(is_missing_key.then(|| {
        div().flex().child(
            button(
                "nx-settings",
                ButtonKind::Secondary,
                t!("mods.nexus.open_settings"),
                false,
            )
            .on_click(|_, _, cx| crate::views::settings::open(crate::views::settings::Tab::General, cx)),
        )
    }))
}

fn remaining_line() -> Option<Div> {
    let left = decanter_nexus::remaining();
    let (Some(daily), Some(hourly)) = (left.daily, left.hourly) else {
        return None;
    };
    Some(theme::muted(t!("mods.nexus.remaining", daily = daily, hourly = hourly)).text_size(px(12.)))
}

fn feed_label(feed: Feed) -> SharedString {
    match feed {
        Feed::Trending => t!("mods.nexus.feed_trending"),
        Feed::Updated => t!("mods.nexus.feed_updated"),
        Feed::Latest | _ => t!("mods.nexus.feed_latest"),
    }
    .into_owned()
    .into()
}

fn results_panel(browser: &Browser, cx: &Context<ModsTab>) -> Div {
    let entity = cx.entity();
    let feeds = [Feed::Latest, Feed::Trending, Feed::Updated];
    let selected = match browser.feed {
        Feed::Trending => "trending",
        Feed::Updated => "updated",
        Feed::Latest | _ => "latest",
    };
    let feed = Select::new(
        "nx-feed",
        feeds.map(|feed| {
            let value = match feed {
                Feed::Trending => "trending",
                Feed::Updated => "updated",
                Feed::Latest | _ => "latest",
            };
            (SharedString::from(value), feed_label(feed))
        }),
        selected,
    )
    .on_change(move |chosen, _, cx| {
        let feed = match chosen.as_ref() {
            "trending" => Feed::Trending,
            "updated" => Feed::Updated,
            _ => Feed::Latest,
        };
        entity.update(cx, |this, cx| {
            if let Some(browser) = this.nexus.as_mut() {
                browser.feed = feed;
            }
            this.load_feed(cx);
        });
    });
    let filter = browser.filter.read(cx).text().trim().to_lowercase();
    let words: Vec<&str> = filter.split_whitespace().collect();
    let shown: Vec<&ModInfo> = browser
        .mods
        .iter()
        .filter(|info| {
            let name = info.name.to_lowercase();
            let summary = info.summary.to_lowercase();
            words.iter().all(|word| name.contains(word) || summary.contains(word))
        })
        .collect();
    let count = t!("mods.nexus.results", count = shown.len(), total = browser.mods.len());
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            field_row()
                .items_center()
                .child(div().w(px(110.)).child(theme::muted(t!("mods.nexus.feed"))))
                .child(feed),
        )
        .child(
            field_row()
                .items_center()
                .child(div().flex_1().min_w_0().child(browser.filter.clone())),
        )
        .child(
            field_row()
                .items_center()
                .child(div().flex_1().min_w_0().child(browser.reference.clone()))
                .child(
                    button(
                        "nx-open-ref",
                        ButtonKind::Secondary,
                        t!("mods.nexus.open_reference"),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.open_reference(cx))),
                ),
        )
        .child(theme::muted(t!("mods.nexus.search_note")))
        .child(theme::muted(count))
        .children(
            shown
                .into_iter()
                .enumerate()
                .map(|(index, info)| result_row(index, info, cx)),
        )
}

fn header(browser: &Browser, working: bool, cx: &Context<ModsTab>) -> Div {
    let game = browser.settings.as_ref().map(|settings| {
        let name = browser
            .games
            .as_ref()
            .and_then(|all| all.iter().find(|known| known.domain == settings.domain))
            .map_or_else(|| settings.domain.to_string(), |known| known.name.clone());
        theme::muted(t!("mods.nexus.game", name = name))
    });
    let can_change = browser.settings.is_some() && !browser.choosing && browser.account.is_some();
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(div().flex_1().child(t!("mods.nexus.title")))
        .children(game)
        .children(can_change.then(|| {
            button("nx-change", ButtonKind::Secondary, t!("mods.nexus.change"), working).when(!working, |b| {
                b.on_click(cx.listener(|this, _, _, cx| {
                    let needs_list = this.nexus.as_mut().is_some_and(|browser| {
                        browser.choosing = true;
                        browser.games.is_none()
                    });
                    if needs_list {
                        this.load_games(Refresh::IfStale, cx);
                    }
                    cx.notify();
                }))
            })
        }))
        .child(
            button("nx-close", ButtonKind::Secondary, t!("mods.nexus.close"), false)
                .on_click(cx.listener(|this, _, _, cx| this.close_nexus(cx))),
        )
}

fn picture(url: Option<&str>, size: f32) -> Div {
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

fn stats(info: &ModInfo) -> String {
    t!(
        "mods.nexus.stats",
        author = info.author.as_deref().unwrap_or("-"),
        version = info.version.as_deref().unwrap_or("-"),
        endorsements = info.endorsements,
        updated = info.updated.as_deref().unwrap_or("-")
    )
    .into_owned()
}

fn flags(info: &ModInfo) -> Option<Div> {
    let mut flags = Vec::new();
    if !info.is_available {
        flags.push(t!("mods.nexus.unavailable").into_owned());
    }
    if info.has_adult_content {
        flags.push(t!("mods.nexus.adult").into_owned());
    }
    (!flags.is_empty()).then(|| theme::error(flags.join(" · ")))
}

fn result_row(index: usize, info: &ModInfo, cx: &Context<ModsTab>) -> impl IntoElement + use<> {
    let id = info.id;
    div()
        .id(("nx-result", index))
        .flex()
        .gap_3()
        .py(px(6.))
        .cursor_pointer()
        .border_t_1()
        .border_color(rgb(theme::LINE))
        .on_click(cx.listener(move |this, _, _, cx| this.open_mod(id, cx)))
        .child(picture(info.picture.as_deref(), 40.))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(info.name.clone())
                .child(theme::muted(summary(&info.summary)).text_size(px(12.5)))
                .child(theme::muted(stats(info)).text_size(px(12.)))
                .children(flags(info)),
        )
}

fn category_label(category: Category) -> String {
    match category {
        Category::Main => t!("mods.nexus.category_main"),
        Category::Update => t!("mods.nexus.category_update"),
        Category::Optional => t!("mods.nexus.category_optional"),
        Category::Miscellaneous => t!("mods.nexus.category_misc"),
        Category::OldVersion => t!("mods.nexus.category_old"),
        Category::Archived => t!("mods.nexus.category_archived"),
        Category::Deleted => t!("mods.nexus.category_deleted"),
        Category::Other | _ => t!("mods.nexus.category_other"),
    }
    .into_owned()
}

fn file_row(
    index: usize,
    browser: &Browser,
    selected: &Selected,
    file: &NexusFile,
    locked: bool,
    cx: &Context<ModsTab>,
) -> Div {
    let size = file
        .shown_size()
        .map_or_else(|| t!("mods.nexus.size_unknown").into_owned(), crate::format::bytes);
    let md5 = if file.md5.is_some() {
        t!("mods.nexus.md5_listed")
    } else {
        t!("mods.nexus.md5_missing")
    };
    let line = t!(
        "mods.nexus.file_line",
        category = category_label(file.category),
        version = file.version.as_deref().unwrap_or("-"),
        size = size,
        date = file.uploaded_date.as_deref().unwrap_or("-"),
        md5 = md5
    );
    let can_download = browser.is_premium() && file.category != Category::Deleted;
    let mod_id = selected.info.id;
    let file_id = file.id;
    div()
        .flex()
        .items_center()
        .gap_3()
        .pt_1()
        .border_t_1()
        .border_color(rgb(theme::LINE))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(file.name.clone())
                .child(theme::muted(line).text_size(px(12.)))
                .children(
                    (!file.description.is_empty()).then(|| theme::muted(summary(&file.description)).text_size(px(12.))),
                ),
        )
        .children(can_download.then(|| {
            button(
                ("nx-download", index),
                ButtonKind::Primary,
                t!("mods.nexus.download"),
                locked,
            )
            .when(!locked, |b| {
                b.on_click(cx.listener(move |this, _, _, cx| this.download_file(mod_id, file_id, cx)))
            })
        }))
}

fn detail(browser: &Browser, selected: &Selected, locked: bool, cx: &Context<ModsTab>) -> Div {
    let info = &selected.info;
    let page = browser.domain().map(|domain| decanter_nexus::mod_page(domain, info.id));
    let free_line = (!browser.is_premium()).then(|| div().child(t!("mods.nexus.free_account")));
    let files = if selected.files.files.is_empty() {
        vec![theme::muted(t!("mods.nexus.no_files"))]
    } else {
        selected
            .files
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| file_row(index, browser, selected, file, locked, cx))
            .collect()
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div().flex().gap_3().child(picture(info.picture.as_deref(), 64.)).child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(info.name.clone())
                    .child(theme::muted(stats(info)).text_size(px(12.)))
                    .children(flags(info)),
            ),
        )
        .child(div().child(info.summary.clone()))
        .children(free_line)
        .child(theme::muted(t!("mods.nexus.files", count = selected.files.files.len())))
        .children(files)
        .child(
            div()
                .flex()
                .gap(px(10.))
                .pt_2()
                .children(page.map(|page| {
                    button("nx-page", ButtonKind::Secondary, t!("mods.nexus.open_page"), false).on_click(
                        move |_, _, _| {
                            if let Err(err) = crate::desktop::open(&page) {
                                tracing::warn!(%err, "could not open the mod's page");
                            }
                        },
                    )
                }))
                .child(
                    button("nx-back", ButtonKind::Secondary, t!("mods.nexus.back"), false).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(browser) = this.nexus.as_mut() {
                                browser.selected = None;
                            }
                            cx.notify();
                        },
                    )),
                ),
        )
}
