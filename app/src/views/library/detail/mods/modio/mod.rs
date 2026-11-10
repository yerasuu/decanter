//! Adding mods from mod.io with the user's own API key: mod.io's terms of use first, accepted by
//! the user's own press, then the game's mod.io game (searched by title, chosen by the user and
//! remembered beside its mods, its own record checked for third-party access every time), a page
//! of its mods, a mod's files and dependencies, and installing one file through the same import
//! as any archive (docs/mod-platforms-analysis.md, section 3). The network is `decanter_modio`'s.
//!
//! Every search is a request, so a page is kept in memory for ten minutes (design, section 2.4):
//! going back to a sort or a query already asked shows it again without asking mod.io.

mod install;
mod updates;

use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use decanter_modio::{
    Client, Detail, Error, Game, GameId, GameSettings, ModFile, ModioModId, ModsPage, ModsQuery, ModsSort, Origin,
    RemoteMod, TagGroup, Terms, Virus,
};
use decanter_mods::Likeness;
use gpui::{Context, Div, Entity, SharedString, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::ModsTab;
use super::credits::{self, Platform};
use super::search::{self, Provider, ResultItem};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::detail::mods::field_row;
use crate::widgets::input::{InputEvent, TextInput};
use crate::widgets::{ButtonKind, button};
pub(super) use install::Install;
pub(super) use updates::UpdateCheck;

const SUMMARY_CHARS: usize = 140;
const PAGE_TTL: Duration = Duration::from_mins(10);
const MAX_PAGES: usize = 40;

pub(super) struct Browser {
    client: Option<Client>,
    data_dir: PathBuf,
    /// mod.io's terms, while they wait for the user's answer.
    terms: Option<Terms>,
    is_agreed: bool,
    settings: Option<GameSettings>,
    /// The game's own record, read fresh and allowing third-party access.
    game: Option<Game>,
    games: Option<Vec<Game>>,
    /// Whether `games` answers words the user typed, which shows what does not look like the
    /// game too, under its own heading.
    games_by_hand: bool,
    /// The game picker is shown: no choice yet, or the user asked to change it.
    choosing: bool,
    game_query: Entity<TextInput>,
    /// The game's tags to filter by, `None` when mod.io did not give them.
    tags: Option<Vec<TagGroup>>,
    /// The sort and filters the next search takes; its words come from the search surface.
    controls: ModsQuery,
    /// What the results answer, which *Mostrar más* goes on with.
    searched: Option<ModsQuery>,
    results: Vec<RemoteMod>,
    total: Option<u64>,
    /// The offset of the next page, while there is one.
    next: Option<u64>,
    pages: Vec<CachedPage>,
    selected: Option<Selected>,
    /// A mod to open once the game is read, from an update check.
    pending: Option<ModioModId>,
    loading: Option<Text>,
    failure: Option<Failure>,
    pub(super) install: Option<Install>,
}

struct Selected {
    remote: RemoteMod,
    detail: Option<Detail>,
}

struct CachedPage {
    query: ModsQuery,
    asked: Instant,
    page: ModsPage,
}

struct Failure {
    text: Text,
    retry: Option<Retry>,
    /// mod.io asked for its terms again: the dialog's links go with the line.
    is_terms: bool,
}

#[derive(Clone, Copy)]
enum Retry {
    Start,
    Games,
    Game(GameId),
    Page(u64),
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

    fn cached(&self, query: &ModsQuery, offset: u64) -> Option<ModsPage> {
        self.pages
            .iter()
            .find(|cached| cached.page.offset == offset && cached.query == *query && cached.asked.elapsed() < PAGE_TTL)
            .map(|cached| cached.page.clone())
    }

    fn remember(&mut self, query: ModsQuery, page: ModsPage) {
        self.pages.retain(|cached| cached.asked.elapsed() < PAGE_TTL);
        if self.pages.len() >= MAX_PAGES {
            self.pages.remove(0);
        }
        self.pages.push(CachedPage {
            query,
            asked: Instant::now(),
            page,
        });
    }

    /// A first page replaces the results, a later one adds to them.
    fn show(&mut self, query: ModsQuery, page: ModsPage) {
        let count = page.mods.len() as u64;
        let end = page.offset.saturating_add(count);
        let has_more = count > 0
            && page
                .total
                .map_or(count == decanter_modio::PAGE_SIZE, |total| end < total);
        if page.offset == 0 {
            self.results = page.mods;
        } else {
            self.results.extend(page.mods);
        }
        self.total = page.total;
        self.next = has_more.then_some(end);
        self.searched = Some(query);
    }
}

impl ModsTab {
    pub(super) fn open_modio(&mut self, pending: Option<ModioModId>, cx: &mut Context<Self>) {
        self.show_search(Provider::Modio, cx);
        if let Some(browser) = self.modio.as_mut() {
            if let Some(id) = pending {
                browser.pending = Some(id);
                if browser.game.is_some() {
                    self.modio_open_pending(cx);
                }
            }
            return;
        }
        let Some(mods) = self.mods.as_ref() else {
            return;
        };
        let Some(data_dir) = decanter_modio::data_dir(mods) else {
            tracing::warn!("no modio data folder for the game's library");
            return;
        };
        let settings = match GameSettings::read(mods) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(%err, "modio game settings unreadable, asking again");
                None
            }
        };
        let game_query = cx.new(|cx| TextInput::new(Text::new("mods.modio.game_search"), cx));
        cx.subscribe(&game_query, |this, input, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit) {
                let query = input.read(cx).text().trim().to_owned();
                this.modio_find_games(query, true, cx);
            }
        })
        .detach();
        tracing::info!(has_settings = settings.is_some(), "modio browser opened");
        self.modio = Some(Browser {
            client: None,
            data_dir,
            terms: None,
            is_agreed: false,
            choosing: settings.is_none(),
            settings,
            game: None,
            games: None,
            games_by_hand: false,
            game_query,
            tags: None,
            controls: ModsQuery::new(),
            searched: None,
            results: Vec::new(),
            total: None,
            next: None,
            pages: Vec::new(),
            selected: None,
            pending,
            loading: None,
            failure: None,
            install: None,
        });
        self.modio_start(cx);
    }

    /// Runs `work` off the UI with `loading` shown, then `done` with its answer; a failure becomes
    /// the browser's line, with `retry` when asking again may help.
    fn modio_task<T, F>(
        &mut self,
        loading: &'static str,
        retry: Option<Retry>,
        work: F,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) where
        T: Send + 'static,
        F: Future<Output = decanter_modio::Result<T>> + Send + 'static,
    {
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        browser.loading = Some(Text::new(loading));
        browser.failure = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = tokio::spawn(work).await;
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.modio.as_mut() else {
                    return;
                };
                browser.loading = None;
                match answer {
                    Ok(Ok(value)) => done(this, value, cx),
                    Ok(Err(err)) => this.modio_fail(&err, retry),
                    Err(err) => {
                        tracing::error!(%err, "modio task stopped");
                        browser.failure = Some(Failure {
                            text: Text::verbatim(err),
                            retry,
                            is_terms: false,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows `err`; mod.io asking for its terms drops the agreement, so the next start shows them.
    fn modio_fail(&mut self, err: &Error, retry: Option<Retry>) {
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        tracing::warn!(%err, retryable = err.is_retryable(), "modio request failed");
        let is_terms = matches!(err, Error::TermsRequired { .. });
        if is_terms {
            browser.is_agreed = false;
            if let Err(cleanup) = decanter_modio::forget_agreement(&browser.data_dir) {
                tracing::error!(%cleanup, "modio terms agreement could not be dropped");
            }
        }
        browser.failure = Some(Failure {
            text: crate::modio::failure(err),
            retry: if is_terms {
                Some(Retry::Start)
            } else {
                retry.filter(|_| err.is_retryable())
            },
            is_terms,
        });
    }

    /// The key, then mod.io's terms, and whether the user already agreed to these.
    fn modio_start(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        browser.loading = Some(Text::new("mods.modio.loading_key"));
        browser.failure = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let client = tokio::task::spawn_blocking(crate::modio::client)
                .await
                .unwrap_or_else(|err| Err(Text::verbatim(err)));
            let _ = this.update(cx, |this, cx| {
                let Some(browser) = this.modio.as_mut() else {
                    return;
                };
                browser.loading = None;
                match client {
                    Ok(client) => {
                        browser.client = Some(client);
                        this.modio_read_terms(cx);
                    }
                    Err(text) => {
                        browser.failure = Some(Failure {
                            text,
                            retry: Some(Retry::Start),
                            is_terms: false,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn modio_read_terms(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.modio.as_ref() else {
            return;
        };
        let Some(client) = browser.client.clone() else {
            return;
        };
        let dir = browser.data_dir.clone();
        self.modio_task(
            "mods.modio.loading_terms",
            Some(Retry::Start),
            async move {
                let terms = decanter_modio::terms(&client).await?;
                let is_agreed = decanter_modio::agreed(&dir, &terms)?;
                Ok((terms, is_agreed))
            },
            |this, (terms, is_agreed), cx| {
                let Some(browser) = this.modio.as_mut() else {
                    return;
                };
                if is_agreed {
                    browser.is_agreed = true;
                    this.modio_after_terms(cx);
                } else {
                    tracing::info!("modio terms shown, waiting for the user");
                    browser.terms = Some(terms);
                }
            },
            cx,
        );
    }

    fn modio_agree(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        let Some(terms) = browser.terms.take() else {
            return;
        };
        if let Err(err) = decanter_modio::record_agreement(&browser.data_dir, &terms) {
            tracing::error!(%err, "modio terms agreement could not be saved");
            browser.terms = Some(terms);
            self.modio_fail(&err, None);
            cx.notify();
            return;
        }
        browser.is_agreed = true;
        self.modio_after_terms(cx);
        cx.notify();
    }

    fn modio_disagree(&mut self, cx: &mut Context<Self>) {
        tracing::info!("modio terms declined by the user");
        self.close_modio(cx);
        self.notice = Some(Text::new("mods.modio.terms.declined"));
    }

    fn modio_after_terms(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.modio.as_ref() else {
            return;
        };
        match browser.settings.as_ref().map(|settings| settings.game) {
            Some(game) if !browser.choosing => self.modio_load_game(game, cx),
            _ => {
                let title = self.target.title.clone();
                self.modio_find_games(title, false, cx);
            }
        }
    }

    fn modio_find_games(&mut self, query: String, by_hand: bool, cx: &mut Context<Self>) {
        let Some(client) = self.modio.as_ref().and_then(|browser| browser.client.clone()) else {
            return;
        };
        if query.trim().is_empty() {
            return;
        }
        self.modio_task(
            "mods.modio.loading_games",
            Some(Retry::Games),
            async move { decanter_modio::search_games(&client, &query).await },
            move |this, games, _| {
                if let Some(browser) = this.modio.as_mut() {
                    browser.games = Some(games);
                    browser.games_by_hand = by_hand;
                    browser.choosing = true;
                }
            },
            cx,
        );
    }

    /// Reads the chosen game's own record; [`Self::modio_load_game`] saves the choice only once
    /// that record answered and lets decanter in.
    fn modio_choose_game(&mut self, game: &Game, cx: &mut Context<Self>) {
        if let Err(err) = decanter_modio::check_access(game) {
            self.modio_fail(&err, None);
            cx.notify();
            return;
        }
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        browser.choosing = false;
        browser.game = None;
        browser.results.clear();
        browser.searched = None;
        browser.pages.clear();
        browser.selected = None;
        self.modio_load_game(game.id, cx);
    }

    /// The game's own record, refused unless it allows third-party access, and its tags, read once
    /// for the session; then the choice is saved, if new, and its first page asked. A record mod.io
    /// no longer has drops the choice and shows the picker again.
    fn modio_load_game(&mut self, id: GameId, cx: &mut Context<Self>) {
        let Some(client) = self.modio.as_ref().and_then(|browser| browser.client.clone()) else {
            return;
        };
        let known_tags = self.modio.as_ref().and_then(|browser| {
            browser.game.as_ref().filter(|game| game.id == id)?;
            browser.tags.clone()
        });
        self.modio_task(
            "mods.modio.loading_game",
            Some(Retry::Game(id)),
            async move {
                let game = match decanter_modio::game(&client, id).await {
                    Err(Error::NotFound { .. }) => return Ok(None),
                    found => found?,
                };
                decanter_modio::check_access(&game)?;
                let tags = match known_tags {
                    Some(tags) => Some(tags),
                    // The tags only add a filter, so a failure leaves it out instead of the game.
                    None => match decanter_modio::tag_options(&client, id).await {
                        Ok(tags) => Some(tags),
                        Err(err) => {
                            tracing::warn!(%err, game_id = %id, "modio tags not read, no tag filter");
                            None
                        }
                    },
                };
                Ok(Some((game, tags)))
            },
            move |this, found, cx| {
                let Some((game, tags)) = found else {
                    this.modio_game_gone(id, cx);
                    return;
                };
                let mods = this.mods.clone();
                if let Some(browser) = this.modio.as_mut() {
                    if browser
                        .settings
                        .as_ref()
                        .is_none_or(|settings| settings.game != game.id)
                        && let Some(mods) = mods
                    {
                        let settings = GameSettings::new(game.id, &game.name);
                        match settings.write(&mods) {
                            Ok(()) => browser.settings = Some(settings),
                            Err(err) => {
                                tracing::error!(%err, "modio game settings could not be saved");
                                browser.failure = Some(Failure {
                                    text: crate::modio::failure(&err),
                                    retry: None,
                                    is_terms: false,
                                });
                            }
                        }
                    }
                    browser.game = Some(game);
                    browser.tags = tags.filter(|groups| !groups.is_empty());
                }
                this.modio_search(0, cx);
                this.modio_open_pending(cx);
            },
            cx,
        );
    }

    /// mod.io has no game `id` any more: a saved choice of it is forgotten and the picker shows
    /// again, inside the section, with the title's matches.
    fn modio_game_gone(&mut self, id: GameId, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        let was_saved = browser.settings.as_ref().is_some_and(|settings| settings.game == id);
        tracing::warn!(game_id = %id, was_saved, "modio game record not found, choosing again");
        if was_saved {
            browser.settings = None;
            if let Err(err) = GameSettings::forget(&mods) {
                tracing::error!(%err, "modio game settings could not be forgotten");
            }
        }
        browser.game = None;
        browser.choosing = true;
        browser.results.clear();
        browser.searched = None;
        browser.selected = None;
        let title = self.target.title.clone();
        self.modio_find_games(title, false, cx);
        if let Some(browser) = self.modio.as_mut() {
            browser.failure = Some(Failure {
                text: Text::new("mods.search.mapping_gone").arg("platform", Platform::Modio.name()),
                retry: None,
                is_terms: false,
            });
        }
    }

    /// The first page of what the search surface and the controls ask, or the page at `offset` of
    /// what the results already answer; one kept from the last ten minutes is shown again unasked.
    pub(super) fn modio_search(&mut self, offset: u64, cx: &mut Context<Self>) {
        let text = self.search_text(cx);
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        let (Some(client), Some(game)) = (browser.client.clone(), browser.game.as_ref().map(|game| game.id)) else {
            return;
        };
        let query = if offset == 0 {
            let mut query = browser.controls.clone();
            query.text = text;
            query
        } else if let Some(searched) = browser.searched.clone() {
            searched
        } else {
            return;
        };
        if let Some(page) = browser.cached(&query, offset) {
            tracing::debug!(offset, "modio page shown from the last ten minutes");
            browser.show(query, page);
            browser.failure = None;
            cx.notify();
            return;
        }
        let asked = query.clone();
        self.modio_task(
            "mods.modio.loading_mods",
            Some(Retry::Page(offset)),
            async move { decanter_modio::search_mods(&client, game, &asked, offset).await },
            move |this, page, _| {
                if let Some(browser) = this.modio.as_mut() {
                    browser.remember(query.clone(), page.clone());
                    browser.show(query, page);
                }
            },
            cx,
        );
    }

    /// Changes the sort or a filter, then asks for the first page again.
    fn modio_refine(&mut self, change: impl FnOnce(&mut ModsQuery), cx: &mut Context<Self>) {
        if let Some(browser) = self.modio.as_mut() {
            change(&mut browser.controls);
        }
        self.modio_search(0, cx);
    }

    /// The version of mod `id` the game has from mod.io, if any.
    fn modio_installed(&self, game: GameId, id: ModioModId) -> Option<String> {
        let origin = self.installed_origin(|origin| {
            Origin::from_mod_origin(origin).is_some_and(|found| found.game == game && found.mod_id == id)
        })?;
        Some(Origin::version_of(origin).map_or_else(|| origin.version.clone(), str::to_owned))
    }

    fn modio_select(&mut self, remote: RemoteMod, cx: &mut Context<Self>) {
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        let Some(client) = browser.client.clone() else {
            return;
        };
        let (game, id) = (remote.game, remote.id);
        browser.selected = Some(Selected { remote, detail: None });
        self.modio_task(
            "mods.modio.loading_detail",
            None,
            async move { decanter_modio::detail(&client, game, id).await },
            move |this, detail, _| {
                if let Some(selected) = this
                    .modio
                    .as_mut()
                    .and_then(|browser| browser.selected.as_mut())
                    .filter(|selected| selected.remote.id == id)
                {
                    selected.detail = Some(detail);
                }
            },
            cx,
        );
    }

    /// Opens a mod by its id: a dependency, or the mod an update check found newer.
    fn modio_open(&mut self, id: ModioModId, cx: &mut Context<Self>) {
        let Some(browser) = self.modio.as_ref() else {
            return;
        };
        let (Some(client), Some(game)) = (browser.client.clone(), browser.game.as_ref().map(|game| game.id)) else {
            return;
        };
        self.modio_task(
            "mods.modio.loading_detail",
            None,
            async move { decanter_modio::fetch_mod(&client, game, id).await },
            Self::modio_select,
            cx,
        );
    }

    fn modio_open_pending(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.modio.as_mut().and_then(|browser| browser.pending.take()) {
            self.modio_open(id, cx);
        }
    }

    fn modio_retry(&mut self, retry: Retry, cx: &mut Context<Self>) {
        match retry {
            Retry::Start => {
                if let Some(browser) = self.modio.as_mut() {
                    browser.failure = None;
                }
                self.modio_start(cx);
            }
            Retry::Games => {
                let query = self.modio.as_ref().map_or_else(String::new, |browser| {
                    browser.game_query.read(cx).text().trim().to_owned()
                });
                let by_hand = !query.is_empty();
                let query = if by_hand { query } else { self.target.title.clone() };
                self.modio_find_games(query, by_hand, cx);
            }
            Retry::Game(game) => self.modio_load_game(game, cx),
            Retry::Page(offset) => self.modio_search(offset, cx),
        }
    }

    fn close_modio(&mut self, cx: &mut Context<Self>) {
        if let Some(browser) = self.modio.take() {
            browser.discard();
        }
        tracing::info!("modio browser closed");
        cx.notify();
    }

    pub(super) fn modio_panel(&self, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let browser = self.modio.as_ref()?;
        let body = if let Some(terms) = browser.terms.as_ref() {
            terms_panel(terms, cx)
        } else if !browser.is_agreed {
            div()
        } else if browser.choosing {
            self.modio_game_picker(browser, cx)
        } else {
            let chosen = if let Some(install) = browser.install.as_ref() {
                install::panel(install, cx)
            } else if let Some(selected) = browser.selected.as_ref() {
                detail_panel(selected, locked, cx)
            } else {
                search::nothing_chosen()
            };
            let (controls, list) = self.modio_results(browser, cx);
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(self.query_bar(Provider::Modio, controls, cx))
                .child(search::split(list, chosen))
        };
        let failure = browser.failure.as_ref().map(|failure| {
            let retry = failure
                .retry
                .map(|retry| -> search::Retry { Box::new(move |this, cx| this.modio_retry(retry, cx)) });
            let links = failure
                .is_terms
                .then(|| legal_links(decanter_modio::TERMS_PAGE, decanter_modio::PRIVACY_PAGE));
            search::failure_line("modio-retry", &failure.text, retry, links, cx)
        });
        Some(search::section_frame(
            Platform::Modio,
            header(browser, cx),
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(theme::muted(t!("mods.modio.note")).text_size(px(12.5)))
                .children(browser.loading.as_ref().map(theme::muted))
                .children(failure)
                .child(body),
        ))
    }

    fn modio_game_picker(&self, browser: &Browser, cx: &Context<Self>) -> Div {
        let title = &self.target.title;
        let games = browser.games.as_deref().unwrap_or_default();
        // mod.io's own search answers loosely, so only what looks like the game is suggested; the
        // rest shows only for words the user typed, under its own heading.
        let mut likely: Vec<(Likeness, &Game)> = games
            .iter()
            .filter_map(|game| Some((decanter_mods::likeness(title, &game.name)?, game)))
            .collect();
        likely.sort_by_key(|&(found, _)| core::cmp::Reverse(found));
        let exact = likely.iter().filter(|&&(found, _)| found == Likeness::Exact).count();
        let others: Vec<&Game> = if browser.games_by_hand {
            games
                .iter()
                .filter(|game| decanter_mods::likeness(title, &game.name).is_none())
                .collect()
        } else {
            Vec::new()
        };
        let intro = if browser.games.is_none() {
            t!("mods.modio.game_type", title = title)
        } else if likely.is_empty() {
            t!("mods.search.no_match", platform = Platform::Modio.name())
        } else if exact == 1 {
            t!("mods.modio.game_exact", title = title)
        } else {
            t!("mods.modio.game_ambiguous", title = title)
        };
        let input = browser.game_query.clone();
        let suggested = likely.len();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(theme::muted(intro))
            .children(
                likely
                    .into_iter()
                    .map(|(_, game)| game)
                    .chain(others)
                    .enumerate()
                    .map(|(index, game)| {
                        let heading = (index == suggested).then(search::other_results);
                        let access = if !game.access.allows_third_party() {
                            theme::error(t!("mods.modio.access_off"))
                        } else if game.access.needs_login() {
                            theme::muted(t!("mods.modio.access_login"))
                        } else {
                            theme::muted(t!("mods.modio.access_on"))
                        };
                        let allowed = game.access.allows_third_party();
                        let chosen = game.clone();
                        let row = div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .py(px(4.))
                            .child(search::picture(game.logo.as_deref(), 32.))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .child(game.name.clone())
                                    .child(access.text_size(px(12.))),
                            )
                            .child(
                                button(
                                    ("modio-game", index),
                                    ButtonKind::Secondary,
                                    t!("mods.modio.use"),
                                    !allowed,
                                )
                                .when(allowed, |b| {
                                    b.on_click(cx.listener(move |this, _, _, cx| this.modio_choose_game(&chosen, cx)))
                                }),
                            );
                        div().flex().flex_col().children(heading).child(row)
                    }),
            )
            .child(
                field_row()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(browser.game_query.clone()))
                    .child(
                        button(
                            "modio-game-search",
                            ButtonKind::Secondary,
                            t!("mods.modio.search_button"),
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let query = input.read(cx).text().trim().to_owned();
                            this.modio_find_games(query, true, cx);
                        })),
                    ),
            )
    }

    /// The sort and filter controls, then the results under them.
    fn modio_results(&self, browser: &Browser, cx: &Context<Self>) -> (Div, Div) {
        let sorts = vec![
            (
                ModsSort::Downloads,
                t!("mods.search.sort_downloads").into_owned().into(),
            ),
            (ModsSort::Updated, t!("mods.search.sort_updated").into_owned().into()),
            (ModsSort::Newest, t!("mods.search.sort_newest").into_owned().into()),
            (ModsSort::Rating, t!("mods.search.sort_rating").into_owned().into()),
            (ModsSort::Name, t!("mods.search.sort_name").into_owned().into()),
        ];
        let sort = search::choice(
            "modio-sort",
            t!("mods.search.sort").into_owned().into(),
            sorts,
            browser.controls.sort,
            |this, sort, cx| this.modio_refine(|query| query.sort = sort, cx),
            cx,
        );
        let tags: Vec<(usize, SharedString)> = browser
            .tags
            .iter()
            .flatten()
            .flat_map(|group| group.tags.iter().map(move |tag| format!("{}: {tag}", group.name)))
            .enumerate()
            .map(|(index, label)| (index, SharedString::from(label)))
            .collect();
        let tag = (!tags.is_empty()).then(|| {
            let all: Vec<&String> = browser.tags.iter().flatten().flat_map(|group| &group.tags).collect();
            let selected = browser
                .controls
                .tags
                .first()
                .and_then(|wanted| all.iter().position(|tag| *tag == wanted));
            let options = core::iter::once((None, SharedString::from(t!("mods.search.any").into_owned())))
                .chain(tags.into_iter().map(|(index, label)| (Some(index), label)))
                .collect();
            search::choice(
                "modio-tag",
                t!("mods.search.tag").into_owned().into(),
                options,
                selected,
                |this, chosen: Option<usize>, cx| {
                    let tag = chosen.and_then(|index| {
                        this.modio
                            .as_ref()?
                            .tags
                            .iter()
                            .flatten()
                            .flat_map(|group| &group.tags)
                            .nth(index)
                            .cloned()
                    });
                    this.modio_refine(|query| query.tags = tag.into_iter().collect(), cx);
                },
                cx,
            )
        });
        let controls = search::controls().child(sort).children(tag).child(search::flag(
            "modio-hide-mature",
            t!("mods.search.hide_mature").into_owned().into(),
            browser.controls.hide_mature,
            |this, on, cx| this.modio_refine(|query| query.hide_mature = on, cx),
            cx,
        ));
        let rows = browser
            .results
            .iter()
            .enumerate()
            .map(|(index, remote)| self.modio_result_row(index, remote, cx));
        let list = div()
            .flex()
            .flex_col()
            .gap_2()
            .children(
                browser
                    .searched
                    .is_some()
                    .then(|| search::count_line(browser.results.len(), browser.total)),
            )
            .children(rows)
            .children(
                browser
                    .next
                    .map(|next| search::load_more("modio-more", move |this, cx| this.modio_search(next, cx), cx)),
            );
        (controls, list)
    }

    fn modio_result_row(&self, index: usize, remote: &RemoteMod, cx: &Context<Self>) -> impl IntoElement + use<> {
        let chosen = remote.clone();
        search::result_row(
            ("modio-result", index),
            ResultItem {
                picture: remote.logo.as_deref(),
                name: &remote.name,
                by: Some(by_line(remote)),
                summary: search::summary(&remote.summary, SUMMARY_CHARS),
                stats: stats(remote),
                flags: Vec::new(),
                platform: Platform::Modio,
                is_adult: remote.maturity != 0,
                installed: self.modio_installed(remote.game, remote.id),
            },
            move |this, cx| this.modio_select(chosen.clone(), cx),
            cx,
        )
    }
}

fn header(browser: &Browser, cx: &Context<ModsTab>) -> Div {
    let working = browser.install.is_some() || browser.loading.is_some();
    let game = browser
        .game
        .as_ref()
        .map(|game| game.name.clone())
        .or_else(|| browser.settings.as_ref().map(|settings| settings.name.clone()));
    let can_change = browser.is_agreed && browser.settings.is_some() && !browser.choosing;
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div().flex_1().children(
                game.filter(|_| !browser.choosing)
                    .map(|name| theme::muted(t!("mods.modio.game", name = name))),
            ),
        )
        .children(can_change.then(|| {
            button("modio-change", ButtonKind::Secondary, t!("mods.modio.change"), working).when(!working, |b| {
                b.on_click(cx.listener(|this, _, _, cx| {
                    let title = this.target.title.clone();
                    if let Some(browser) = this.modio.as_mut() {
                        browser.choosing = true;
                        browser.selected = None;
                    }
                    let has_list = this.modio.as_ref().is_some_and(|browser| browser.games.is_some());
                    if !has_list {
                        this.modio_find_games(title, false, cx);
                    }
                    cx.notify();
                }))
            })
        }))
}

fn legal_links(terms: &str, privacy: &str) -> Div {
    let (terms, privacy) = (terms.to_owned(), privacy.to_owned());
    div()
        .flex()
        .gap(px(10.))
        .child(
            button(
                "modio-terms-page",
                ButtonKind::Secondary,
                t!("mods.modio.terms.open_terms"),
                false,
            )
            .on_click(move |_, _, _| open_page(&terms)),
        )
        .child(
            button(
                "modio-privacy-page",
                ButtonKind::Secondary,
                t!("mods.modio.terms.open_privacy"),
                false,
            )
            .on_click(move |_, _, _| open_page(&privacy)),
        )
}

fn open_page(url: &str) {
    if let Err(err) = crate::desktop::open(url) {
        tracing::warn!(%err, "could not open a mod.io page");
    }
}

fn terms_panel(terms: &Terms, cx: &Context<ModsTab>) -> Div {
    let agree = if terms.agree.is_empty() {
        t!("mods.modio.terms.agree").into_owned()
    } else {
        terms.agree.clone()
    };
    let disagree = if terms.disagree.is_empty() {
        t!("mods.modio.terms.disagree").into_owned()
    } else {
        terms.disagree.clone()
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().child(t!("mods.modio.terms.intro")))
        .child(
            div()
                .id("modio-terms-text")
                .max_h(px(260.))
                .overflow_y_scroll()
                .p_2()
                .rounded(px(theme::RADIUS_SM))
                .bg(rgb(theme::BG_PANEL))
                .text_size(px(12.5))
                .children(terms.text.lines().map(|line| div().child(line.to_owned()))),
        )
        .child(legal_links(&terms.terms_url, &terms.privacy_url))
        .child(
            div()
                .flex()
                .gap(px(10.))
                .child(
                    button("modio-agree", ButtonKind::Primary, agree, false)
                        .on_click(cx.listener(|this, _, _, cx| this.modio_agree(cx))),
                )
                .child(
                    button("modio-disagree", ButtonKind::Secondary, disagree, false)
                        .on_click(cx.listener(|this, _, _, cx| this.modio_disagree(cx))),
                ),
        )
}

fn stats(remote: &RemoteMod) -> String {
    let rating = match (remote.rating.as_deref(), remote.rating_percent) {
        (Some(text), Some(percent)) => format!("{text} ({percent} %)"),
        (Some(text), None) => text.to_owned(),
        (None, Some(percent)) => format!("{percent} %"),
        (None, None) => "-".to_owned(),
    };
    t!(
        "mods.modio.stats",
        downloads = remote.downloads,
        subscribers = remote.subscribers,
        rating = rating,
        updated = remote.updated.as_deref().unwrap_or("-")
    )
    .into_owned()
}

fn by_line(remote: &RemoteMod) -> String {
    let version = remote
        .current
        .as_ref()
        .and_then(|file| file.version.as_deref())
        .unwrap_or("-");
    remote
        .author
        .as_deref()
        .map_or_else(
            || t!("mods.modio.version_only", version = version),
            |author| t!("mods.modio.by", author = author, version = version),
        )
        .into_owned()
}

fn virus_line(file: &ModFile) -> Option<Div> {
    match file.virus {
        Virus::Malicious => Some(theme::error(t!("mods.modio.virus_malicious"))),
        Virus::Harmful => Some(theme::error(t!("mods.modio.virus_harmful"))),
        Virus::Unscanned => Some(theme::muted(t!("mods.modio.virus_unscanned"))),
        Virus::Clean | _ => None,
    }
}

fn file_line(file: &ModFile) -> String {
    let size = file
        .size
        .map_or_else(|| t!("mods.modio.size_unknown").into_owned(), crate::format::bytes);
    t!(
        "mods.modio.file_line",
        name = file.filename,
        version = file.version.as_deref().unwrap_or("-"),
        date = file.added.as_deref().unwrap_or("-"),
        size = size
    )
    .into_owned()
}

fn file_row(
    index: usize,
    remote: &RemoteMod,
    file: &ModFile,
    is_current: bool,
    locked: bool,
    cx: &Context<ModsTab>,
) -> Div {
    let refused = locked || file.virus == Virus::Malicious;
    let (chosen, id) = (remote.clone(), file.id);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .pt_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().flex_1().min_w_0().child(file_line(file)))
                .children(is_current.then(|| theme::muted(t!("mods.modio.current"))))
                .child(
                    button(
                        ("modio-install", index),
                        if is_current {
                            ButtonKind::Primary
                        } else {
                            ButtonKind::Secondary
                        },
                        t!("mods.modio.install"),
                        refused,
                    )
                    .when(!refused, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.modio_install(&chosen, id, cx)))
                    }),
                ),
        )
        .children(
            file.md5
                .is_none()
                .then(|| theme::muted(t!("mods.modio.no_md5")).text_size(px(12.))),
        )
        .children(virus_line(file).map(|line| line.text_size(px(12.))))
        .children(
            (!file.changelog.is_empty())
                .then(|| theme::muted(search::summary(&file.changelog, SUMMARY_CHARS)).text_size(px(12.))),
        )
}

fn detail_panel(selected: &Selected, locked: bool, cx: &Context<ModsTab>) -> Div {
    let remote = &selected.remote;
    let page = remote.page.clone();
    let current = remote.current.as_ref().map(|file| file.id);
    let files: Vec<&ModFile> = match selected.detail.as_ref() {
        Some(detail) if !detail.files.is_empty() => detail.files.iter().collect(),
        _ => remote.current.iter().collect(),
    };
    let files_block = if files.is_empty() {
        theme::muted(t!("mods.modio.no_files"))
    } else {
        div()
            .flex()
            .flex_col()
            .child(theme::muted(t!("mods.modio.files")))
            .children(
                files
                    .into_iter()
                    .enumerate()
                    .map(|(index, file)| file_row(index, remote, file, Some(file.id) == current, locked, cx)),
            )
    };
    let dependencies = match selected.detail.as_ref().map(|detail| detail.dependencies.as_deref()) {
        None => theme::muted(t!("mods.modio.loading_dependencies")),
        Some(None) => theme::muted(t!("mods.modio.dependencies_unknown")),
        Some(Some([])) => theme::muted(t!("mods.modio.no_dependencies")),
        Some(Some(found)) => div()
            .flex()
            .flex_col()
            .gap_1()
            .child(theme::muted(t!("mods.modio.dependencies", count = found.len())))
            .children(found.iter().enumerate().map(|(index, dependency)| {
                let id = dependency.id;
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .pl_2()
                    .child(div().flex_1().child(dependency.name.clone()))
                    .child(
                        button(
                            ("modio-dependency", index),
                            ButtonKind::Secondary,
                            t!("mods.modio.show"),
                            false,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.modio_open(id, cx))),
                    )
            })),
    };
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(search::detail_head(
            remote.logo.as_deref(),
            &remote.name,
            [
                theme::muted(by_line(remote)),
                theme::muted(stats(remote)).text_size(px(12.)),
                theme::muted(t!(
                    "mods.modio.dates",
                    added = remote.added.as_deref().unwrap_or("-"),
                    updated = remote.updated.as_deref().unwrap_or("-")
                ))
                .text_size(px(12.)),
            ],
        ))
        .child(credits::browsed("modio-credits", Platform::Modio, remote.credits()))
        .child(div().child(remote.summary.clone()))
        .child(files_block)
        .child(dependencies)
        .child(div().flex().gap(px(10.)).children(page.map(|page| {
            button(
                "modio-page",
                ButtonKind::Secondary,
                t!("mods.credits.view_on", platform = Platform::Modio.name()),
                false,
            )
            .on_click(move |_, _, _| open_page(&page))
        })))
}
