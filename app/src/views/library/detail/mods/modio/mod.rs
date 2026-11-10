//! Adding mods from mod.io with the user's own API key: mod.io's terms of use first, accepted by
//! the user's own press, then the game's mod.io game (searched by title, chosen by the user and
//! remembered beside its mods, its own record checked for third-party access every time), a page
//! of its mods, a mod's files and dependencies, and installing one file through the same import
//! as any archive (docs/mod-platforms-analysis.md, section 3). The network is `decanter_modio`'s.

mod install;
mod updates;

use std::future::Future;
use std::path::PathBuf;

use decanter_modio::{
    Client, Detail, Error, Game, GameId, GameSettings, ModFile, ModioModId, ModsPage, RemoteMod, Terms, Virus,
};
use gpui::{Context, Div, Entity, ObjectFit, div, img, prelude::*, px, rgb};
use rust_i18n::t;

use super::ModsTab;
use super::credits::{self, Platform};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::detail::mods::field_row;
use crate::widgets::input::{InputEvent, TextInput};
use crate::widgets::{ButtonKind, button};
pub(super) use install::Install;
pub(super) use updates::UpdateCheck;

const SUMMARY_CHARS: usize = 140;

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
    /// The game picker is shown: no choice yet, or the user asked to change it.
    choosing: bool,
    game_query: Entity<TextInput>,
    search: Entity<TextInput>,
    page: Option<ModsPage>,
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
    Game,
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
}

impl ModsTab {
    pub(super) fn open_modio(&mut self, pending: Option<ModioModId>, cx: &mut Context<Self>) {
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
                this.modio_find_games(query, cx);
            }
        })
        .detach();
        let search = cx.new(|cx| TextInput::new(Text::new("mods.modio.search"), cx));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit) {
                this.modio_search(0, cx);
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
            game_query,
            search,
            page: None,
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
                self.modio_find_games(title, cx);
            }
        }
    }

    fn modio_find_games(&mut self, query: String, cx: &mut Context<Self>) {
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
            |this, games, _| {
                if let Some(browser) = this.modio.as_mut() {
                    browser.games = Some(games);
                    browser.choosing = true;
                }
            },
            cx,
        );
    }

    /// Saves the user's choice once the game's record lets decanter in.
    fn modio_choose_game(&mut self, game: &Game, cx: &mut Context<Self>) {
        if let Err(err) = decanter_modio::check_access(game) {
            self.modio_fail(&err, None);
            cx.notify();
            return;
        }
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(browser) = self.modio.as_mut() else {
            return;
        };
        let settings = GameSettings::new(game.id, &game.name);
        if let Err(err) = settings.write(&mods) {
            tracing::error!(%err, "modio game settings could not be saved");
            self.modio_fail(&err, None);
            cx.notify();
            return;
        }
        browser.settings = Some(settings);
        browser.choosing = false;
        browser.game = None;
        browser.page = None;
        browser.selected = None;
        self.modio_load_game(game.id, cx);
    }

    /// The game's own record, refused unless it allows third-party access, then its first page.
    fn modio_load_game(&mut self, id: GameId, cx: &mut Context<Self>) {
        let Some(client) = self.modio.as_ref().and_then(|browser| browser.client.clone()) else {
            return;
        };
        self.modio_task(
            "mods.modio.loading_game",
            Some(Retry::Game),
            async move {
                let game = decanter_modio::game(&client, id).await?;
                decanter_modio::check_access(&game)?;
                Ok(game)
            },
            |this, game, cx| {
                if let Some(browser) = this.modio.as_mut() {
                    browser.game = Some(game);
                }
                this.modio_search(0, cx);
                this.modio_open_pending(cx);
            },
            cx,
        );
    }

    fn modio_search(&mut self, offset: u64, cx: &mut Context<Self>) {
        let Some(browser) = self.modio.as_ref() else {
            return;
        };
        let (Some(client), Some(game)) = (browser.client.clone(), browser.game.as_ref().map(|game| game.id)) else {
            return;
        };
        let query = browser.search.read(cx).text().to_owned();
        self.modio_task(
            "mods.modio.loading_mods",
            Some(Retry::Page(offset)),
            async move { decanter_modio::search_mods(&client, game, &query, offset).await },
            |this, page, _| {
                if let Some(browser) = this.modio.as_mut() {
                    browser.page = Some(page);
                }
            },
            cx,
        );
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
            |this, remote, cx| this.modio_select(remote, cx),
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
                let query = if query.is_empty() {
                    self.target.title.clone()
                } else {
                    query
                };
                self.modio_find_games(query, cx);
            }
            Retry::Game => {
                if let Some(game) = self
                    .modio
                    .as_ref()
                    .and_then(|browser| browser.settings.as_ref())
                    .map(|settings| settings.game)
                {
                    self.modio_load_game(game, cx);
                }
            }
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
        let body = if let Some(install) = browser.install.as_ref() {
            install::panel(install, cx)
        } else if let Some(loading) = browser.loading.as_ref() {
            theme::muted(loading)
        } else if let Some(terms) = browser.terms.as_ref() {
            terms_panel(terms, cx)
        } else if !browser.is_agreed {
            div()
        } else if browser.choosing {
            self.modio_game_picker(browser, cx)
        } else if let Some(selected) = browser.selected.as_ref() {
            detail_panel(selected, locked, cx)
        } else {
            results_panel(browser, cx)
        };
        let failure = browser.failure.as_ref().map(|failure| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(theme::error(&failure.text))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .children(failure.retry.map(|retry| {
                            button("modio-retry", ButtonKind::Secondary, t!("mods.modio.retry"), false)
                                .on_click(cx.listener(move |this, _, _, cx| this.modio_retry(retry, cx)))
                        }))
                        .when(failure.is_terms, |row| {
                            row.child(legal_links(decanter_modio::TERMS_PAGE, decanter_modio::PRIVACY_PAGE))
                        }),
                )
        });
        Some(
            theme::card()
                .gap_3()
                .px(px(18.))
                .py(px(14.))
                .child(header(browser, cx))
                .child(theme::muted(t!("mods.modio.note")))
                .children(failure)
                .child(body)
                .child(credits::footer(Platform::Modio)),
        )
    }

    fn modio_game_picker(&self, browser: &Browser, cx: &Context<Self>) -> Div {
        let title = &self.target.title;
        let games = browser.games.as_deref().unwrap_or_default();
        let exact = games
            .iter()
            .filter(|game| squashed(&game.name) == squashed(title))
            .count();
        let intro = if browser.games.is_none() {
            t!("mods.modio.game_type", title = title)
        } else if games.is_empty() {
            t!("mods.modio.game_none", title = title)
        } else if exact == 1 {
            t!("mods.modio.game_exact", title = title)
        } else {
            t!("mods.modio.game_ambiguous", title = title)
        };
        let input = browser.game_query.clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(theme::muted(intro))
            .children(games.iter().enumerate().map(|(index, game)| {
                let access = if !game.access.allows_third_party() {
                    theme::error(t!("mods.modio.access_off"))
                } else if game.access.needs_login() {
                    theme::muted(t!("mods.modio.access_login"))
                } else {
                    theme::muted(t!("mods.modio.access_on"))
                };
                let allowed = game.access.allows_third_party();
                let chosen = game.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py(px(4.))
                    .child(logo(game.logo.as_deref(), 32.))
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
                    )
            }))
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
                            this.modio_find_games(query, cx);
                        })),
                    ),
            )
    }
}

fn squashed(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
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
        .child(div().flex_1().child(t!("mods.modio.title")))
        .children(
            game.filter(|_| !browser.choosing)
                .map(|name| theme::muted(t!("mods.modio.game", name = name))),
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
                        this.modio_find_games(title, cx);
                    }
                    cx.notify();
                }))
            })
        }))
        .child(
            button("modio-close", ButtonKind::Secondary, t!("mods.modio.close"), false)
                .on_click(cx.listener(|this, _, _, cx| this.close_modio(cx))),
        )
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

fn results_panel(browser: &Browser, cx: &Context<ModsTab>) -> Div {
    let page = browser.page.as_ref();
    let shown = page.map_or(0, |page| page.mods.len());
    let offset = page.map_or(0, |page| page.offset);
    let total = page.and_then(|page| page.total);
    let count = match total {
        Some(total) if shown > 0 => t!(
            "mods.modio.results_range",
            from = offset.saturating_add(1),
            to = offset.saturating_add(shown as u64),
            count = total
        ),
        _ => t!("mods.modio.results", count = shown),
    };
    let page_size = decanter_modio::PAGE_SIZE;
    let has_previous = offset > 0;
    let has_next = total.map_or(shown as u64 == page_size, |total| {
        offset.saturating_add(shown as u64) < total
    });
    let rows = page
        .into_iter()
        .flat_map(|page| page.mods.iter())
        .enumerate()
        .map(|(index, remote)| result_row(index, remote, cx));
    let input = browser.search.clone();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            field_row()
                .items_center()
                .child(div().flex_1().min_w_0().child(input))
                .child(
                    button(
                        "modio-search",
                        ButtonKind::Secondary,
                        t!("mods.modio.search_button"),
                        false,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.modio_search(0, cx))),
                ),
        )
        .child(theme::muted(count))
        .children(rows)
        .child(
            div()
                .flex()
                .gap(px(10.))
                .children(has_previous.then(|| {
                    let previous = offset.saturating_sub(page_size);
                    button(
                        "modio-previous",
                        ButtonKind::Secondary,
                        t!("mods.modio.previous"),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.modio_search(previous, cx)))
                }))
                .children(has_next.then(|| {
                    let next = offset.saturating_add(shown as u64);
                    button("modio-next", ButtonKind::Secondary, t!("mods.modio.next"), false)
                        .on_click(cx.listener(move |this, _, _, cx| this.modio_search(next, cx)))
                })),
        )
}

fn logo(url: Option<&str>, size: f32) -> Div {
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

fn result_row(index: usize, remote: &RemoteMod, cx: &Context<ModsTab>) -> impl IntoElement + use<> {
    let chosen = remote.clone();
    div()
        .id(("modio-result", index))
        .flex()
        .gap_3()
        .py(px(6.))
        .cursor_pointer()
        .border_t_1()
        .border_color(rgb(theme::LINE))
        .on_click(cx.listener(move |this, _, _, cx| this.modio_select(chosen.clone(), cx)))
        .child(logo(remote.logo.as_deref(), 40.))
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
                        .child(remote.name.clone())
                        .child(theme::muted(by_line(remote)))
                        .child(credits::source(Platform::Modio))
                        .children((remote.maturity != 0).then(credits::adult_badge)),
                )
                .child(theme::muted(summary(&remote.summary)).text_size(px(12.5)))
                .child(theme::muted(stats(remote)).text_size(px(12.))),
        )
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
        .children((!file.changelog.is_empty()).then(|| theme::muted(summary(&file.changelog)).text_size(px(12.))))
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
        .child(
            div().flex().gap_3().child(logo(remote.logo.as_deref(), 64.)).child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(remote.name.clone())
                    .child(theme::muted(by_line(remote)))
                    .child(theme::muted(stats(remote)).text_size(px(12.)))
                    .child(
                        theme::muted(t!(
                            "mods.modio.dates",
                            added = remote.added.as_deref().unwrap_or("-"),
                            updated = remote.updated.as_deref().unwrap_or("-")
                        ))
                        .text_size(px(12.)),
                    ),
            ),
        )
        .child(credits::browsed("modio-credits", Platform::Modio, remote.credits()))
        .child(div().child(remote.summary.clone()))
        .child(files_block)
        .child(dependencies)
        .child(
            div()
                .flex()
                .gap(px(10.))
                .children(page.map(|page| {
                    button("modio-page", ButtonKind::Secondary, t!("mods.modio.open_page"), false)
                        .on_click(move |_, _, _| open_page(&page))
                }))
                .child(
                    button("modio-back", ButtonKind::Secondary, t!("mods.modio.back"), false).on_click(cx.listener(
                        |this, _, _, cx| {
                            if let Some(browser) = this.modio.as_mut() {
                                browser.selected = None;
                            }
                            cx.notify();
                        },
                    )),
                ),
        )
}
