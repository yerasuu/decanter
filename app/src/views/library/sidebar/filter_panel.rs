use std::collections::{BTreeSet, HashMap, HashSet};

use decanter_core::Storefront;
use decanter_gog::LibraryEntry;
use gpui::{
    AnyElement, App, BoxShadow, Context, Div, Entity, Focusable as _, FontWeight, KeyDownEvent, SharedString, Stateful,
    Window, div, point, prelude::*, px, rgb,
};
use rust_i18n::t;

use super::{SIDEBAR_WIDTH, SidebarView};
use crate::state::{AppState, DownloadKey};
use crate::theme;
use crate::views::library::data::{Collections, LibraryStats};
use crate::views::library::filters::{
    AchievementState, Filters, PlayState, Sort, belongs_in_the_library, is_installed, pending_install_ids,
};
use crate::widgets::select::SelectStyle;
use crate::widgets::{Select, checkbox};

/// How many entries sit behind each filter option.
///
/// Counted against the chosen product type only, never against the other
/// filters — a count that shrank as you ticked boxes elsewhere would stop
/// answering the question it's there for: is this option worth ticking?
#[derive(Default)]
struct Tallies {
    states: HashMap<PlayState, usize>,
    achievements: HashMap<AchievementState, usize>,
    genres: HashMap<String, usize>,
    collections: HashMap<String, usize>,
    storefronts: HashMap<Storefront, usize>,
    accounts: HashMap<String, usize>,
}

fn count_options(
    entries: &[&LibraryEntry],
    favorites: &HashSet<String>,
    stats: &LibraryStats,
    collections: &Collections,
    pending_installs: &HashSet<DownloadKey>,
) -> Tallies {
    let mut tallies = Tallies::default();
    for entry in entries {
        let id = &entry.game.id;
        let played = stats.seconds(id) > 0;
        for (state, holds) in [
            (PlayState::Installed, is_installed(&entry.game, pending_installs)),
            (PlayState::Played, played),
            (PlayState::Unplayed, !played),
            (PlayState::Favorite, favorites.contains(id)),
            (PlayState::Mod, entry.is_mod),
        ] {
            if holds {
                *tallies.states.entry(state).or_default() += 1;
            }
        }
        *tallies
            .achievements
            .entry(AchievementState::of(stats.completion(id)))
            .or_default() += 1;
        for genre in &entry.game.genres {
            *tallies.genres.entry(genre.clone()).or_default() += 1;
        }
        for collection in &collections.list {
            if collections.contains(&collection.id, id) {
                *tallies.collections.entry(collection.id.clone()).or_default() += 1;
            }
        }
        *tallies.storefronts.entry(entry.game.storefront).or_default() += 1;
        *tallies.accounts.entry(entry.game.account_id.clone()).or_default() += 1;
    }
    tallies
}

/// Every storefront an entry in the library actually belongs to, drawn
/// from the entries rather than hardcoded so a new storefront, or games
/// added from outside any (`Unspecified`), show up here on their own.
fn available_storefronts(entries: &[&LibraryEntry]) -> Vec<Storefront> {
    let mut found: Vec<Storefront> = entries
        .iter()
        .map(|entry| entry.game.storefront)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    found.sort_by_key(|storefront| storefront.as_str_name());
    found
}

fn storefront_label(storefront: Storefront) -> std::borrow::Cow<'static, str> {
    match storefront {
        // A library entry with no storefront is a game added from outside
        // one, which is what the filter calls it.
        Storefront::Unspecified => t!("library.filters.store.external"),
        Storefront::Steam => "Steam".into(),
        Storefront::EpicGames => "Epic Games".into(),
        Storefront::Gog => "GOG".into(),
        Storefront::AmazonGames => "Amazon Games".into(),
        Storefront::EaApp => "EA App".into(),
        Storefront::UbisoftConnect => "Ubisoft Connect".into(),
        Storefront::BattleNet => "Battle.net".into(),
    }
}

struct PanelOptions {
    total: usize,
    matched: usize,
    storefronts: Vec<Storefront>,
    tallies: Tallies,
    genres: Vec<String>,
    collections: Vec<(String, String)>,
    accounts: Vec<(String, String)>,
}

impl SidebarView {
    /// Drawn by the library page itself, over the detail pane: the panel
    /// starts where the sidebar ends, outside the sidebar's own bounds.
    pub fn filter_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.panel_open {
            return None;
        }
        let profile_id = Self::profile(cx)?;
        self.refresh_cache(&profile_id, cx);
        let current = self.model.read(cx).filters.clone();
        let options = self.panel_options(&current, cx);
        let (total, matched) = (options.total, options.matched);
        let panel_width = (window.viewport_size().width.as_f32() - SIDEBAR_WIDTH).min(720.);
        let grid = filter_grid(&cx.entity(), &current, options, panel_width);
        let head = Self::panel_head(&current, cx);
        let foot = self.panel_foot(&current, total, matched, cx);

        let panel = div()
            .id("filter-panel")
            .track_focus(&self.panel_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.set_panel(false, window, cx);
                }
            }))
            .absolute()
            .top_0()
            .left(px(SIDEBAR_WIDTH))
            .w(px(panel_width))
            .max_h_full()
            .overflow_y_scroll()
            .occlude()
            .bg(rgb(theme::BG_PANEL_2))
            .border_r_1()
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .shadow(vec![BoxShadow {
                color: gpui::black().opacity(0.38),
                offset: point(px(18.), px(0.)),
                blur_radius: px(40.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(head)
            .child(grid)
            .child(foot);
        // A full-bleed click target behind the panel, so clicking anywhere
        // outside closes it, the same as every other popup in this view.
        let backdrop = div()
            .id("filter-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_click(cx.listener(|this, _, window, cx| this.set_panel(false, window, cx)));
        Some(
            div()
                .absolute()
                .inset_0()
                .child(backdrop)
                .child(panel)
                .into_any_element(),
        )
    }

    fn panel_options(&self, current: &Filters, cx: &App) -> PanelOptions {
        let state = AppState::get(cx);
        let pending_installs = pending_install_ids(&state.background.read(cx).installing);
        let games = &state.library.read(cx).games;
        let query = self.search.read(cx).text().to_lowercase();
        let kind = self.kind;
        let cache = &self.cache;
        // The denominator for the result line: everything the chosen
        // product type covers, before any filter narrows it — with the
        // same exclusions the list applies, or "X de Y" counts rows
        // nothing can ever show.
        let in_kind: Vec<&LibraryEntry> = games
            .iter()
            .filter(|entry| kind.accepts(entry.kind))
            .filter(|entry| entry.parent_id.is_none())
            .filter(|entry| belongs_in_the_library(entry))
            .filter(|entry| !cache.hidden.contains(&entry.game.id))
            .collect();
        let matched = games
            .iter()
            .filter(|entry| kind.accepts(entry.kind))
            .filter(|entry| !cache.hidden.contains(&entry.game.id))
            .filter(|entry| {
                current.accepts(
                    entry,
                    &cache.favorites,
                    &cache.stats,
                    &cache.collections,
                    &pending_installs,
                )
            })
            .filter(|entry| query.is_empty() || entry.game.title.to_lowercase().contains(&query))
            .count();
        let tallies = count_options(
            &in_kind,
            &cache.favorites,
            &cache.stats,
            &cache.collections,
            &pending_installs,
        );
        PanelOptions {
            total: in_kind.len(),
            matched,
            storefronts: available_storefronts(&in_kind),
            tallies,
            // Drawn from the whole library, not the filtered view — otherwise
            // picking a genre would empty the very picker it was chosen from.
            genres: games
                .iter()
                .flat_map(|entry| entry.game.genres.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            // Manual collections only — a dynamic one is itself a filter.
            collections: cache
                .collections
                .list
                .iter()
                .filter(|collection| collection.filter.is_none())
                .map(|collection| (collection.id.clone(), collection.name.clone()))
                .collect(),
            accounts: cache.accounts.clone(),
        }
    }

    fn panel_head(current: &Filters, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .items_baseline()
            .gap_3()
            .px_6()
            .pt(px(20.))
            .pb_1()
            .child(
                theme::display(22.)
                    .flex_1()
                    .text_color(rgb(theme::TEXT_BRIGHT))
                    .child(t!("library.filters.heading")),
            )
            .when(!current.is_default(), |head| {
                head.child(
                    link_button("filters-clear")
                        .child(t!("library.filters.clear"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Sort survives a reset: it's a view preference, not
                            // a constraint the user is clearing.
                            this.update_filters(cx, |filters| {
                                let sort = filters.sort;
                                *filters = Filters {
                                    sort,
                                    ..Filters::default()
                                };
                            });
                        })),
                )
            })
            .child(
                link_button("filters-close")
                    .child("✕")
                    .on_click(cx.listener(|this, _, window, cx| this.set_panel(false, window, cx))),
            )
    }

    fn panel_foot(&self, current: &Filters, total: usize, matched: usize, cx: &Context<Self>) -> Div {
        let active = current.active_count();
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .px_6()
            .pt(px(14.))
            .pb(px(20.))
            .border_t_1()
            .border_color(rgb(theme::LINE))
            .child(
                div()
                    .flex_1()
                    .text_size(px(12.))
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(match (active, total) {
                        (0, 1) => t!("library.filters.products.one", count = total),
                        (0, _) => t!("library.filters.products.other", count = total),
                        (1, _) => t!("library.filters.matched.one", matched = matched, total = total),
                        _ => t!(
                            "library.filters.matched.other",
                            matched = matched,
                            total = total,
                            active = active
                        ),
                    }),
            )
            .child(self.save_dynamic(cx))
    }

    /// Naming is the only step this adds: the filter itself is already
    /// whatever the panel above is set to.
    fn save_dynamic(&self, cx: &Context<Self>) -> AnyElement {
        if self.saving {
            return div().w(px(220.)).child(self.save_name.clone()).into_any_element();
        }
        div()
            .id("save-dynamic")
            .px_4()
            .py(px(9.))
            .rounded(px(theme::RADIUS_SM))
            .bg(rgb(theme::ACCENT_BLUE))
            .hover(|button| button.bg(rgb(theme::ACCENT_BLUE_HOVER)))
            .text_color(rgb(theme::TEXT_ON_BLUE))
            .text_size(px(12.5))
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .on_click(cx.listener(|this, _, window, cx| {
                this.saving = true;
                window.focus(&this.save_name.focus_handle(cx), cx);
                cx.notify();
            }))
            .child(t!("library.filters.save_dynamic"))
            .into_any_element()
    }
}

fn filter_grid(entity: &Entity<SidebarView>, current: &Filters, options: PanelOptions, panel_width: f32) -> Div {
    let PanelOptions {
        storefronts,
        tallies,
        genres,
        collections,
        accounts,
        ..
    } = options;
    div()
        .grid()
        .grid_cols(3)
        .gap_x(px(26.))
        .gap_y(px(22.))
        .px_6()
        .pt(px(18.))
        .pb(px(22.))
        .child(state_group(entity, current, &tallies))
        .child(achievement_group(entity, current, &tallies))
        .child(order_group(entity, current, &collections, &tallies))
        .when(accounts.len() > 1, |grid| {
            grid.child(account_group(entity, current, accounts, &tallies))
        })
        // Shown with a single storefront too, so the section is already
        // where people look when a second one, or external games, arrive.
        .when(!storefronts.is_empty(), |grid| {
            grid.child(store_group(entity, current, storefronts, &tallies))
        })
        .child(genre_group(entity, current, genres, &tallies, panel_width))
}

fn state_group(entity: &Entity<SidebarView>, current: &Filters, tallies: &Tallies) -> Div {
    filter_group(&t!("library.filters.state.title"), &t!("library.filters.state.note")).children(
        [
            (PlayState::Installed, "installed", t!("library.filters.state.installed")),
            (PlayState::Played, "played", t!("library.filters.state.played")),
            (PlayState::Unplayed, "unplayed", t!("library.filters.state.unplayed")),
            (PlayState::Favorite, "favorite", t!("library.filters.state.favorite")),
            (PlayState::Mod, "mod", t!("library.filters.state.mod")),
        ]
        .into_iter()
        .map(|(state, id, label)| {
            check_row(
                entity,
                format!("check-state-{id}").into(),
                label.into_owned(),
                tallies.states.get(&state).copied().unwrap_or(0),
                current.states.contains(&state),
                move |filters| {
                    if !filters.states.insert(state) {
                        filters.states.remove(&state);
                    }
                },
            )
        }),
    )
}

fn achievement_group(entity: &Entity<SidebarView>, current: &Filters, tallies: &Tallies) -> Div {
    filter_group(
        &t!("library.filters.achievements.title"),
        &t!("library.filters.achievements.note"),
    )
    .children(
        [
            (
                AchievementState::Complete,
                "complete",
                t!("library.filters.achievements.complete"),
            ),
            (
                AchievementState::InProgress,
                "in-progress",
                t!("library.filters.achievements.in_progress"),
            ),
            (
                AchievementState::NotStarted,
                "not-started",
                t!("library.filters.achievements.not_started"),
            ),
            (
                AchievementState::Unmeasured,
                "unmeasured",
                t!("library.filters.achievements.unmeasured"),
            ),
        ]
        .into_iter()
        .map(|(achievement, id, label)| {
            check_row(
                entity,
                format!("check-achievement-{id}").into(),
                label.into_owned(),
                tallies.achievements.get(&achievement).copied().unwrap_or(0),
                current.achievements.contains(&achievement),
                move |filters| {
                    if !filters.achievements.insert(achievement) {
                        filters.achievements.remove(&achievement);
                    }
                },
            )
        }),
    )
}

fn order_group(
    entity: &Entity<SidebarView>,
    current: &Filters,
    collections: &[(String, String)],
    tallies: &Tallies,
) -> Div {
    let sort_entity = entity.clone();
    let order = filter_group(&t!("library.filters.sort.heading"), &t!("library.filters.sort.note")).child(
        Select::new("filter-sort", Sort::options(), current.sort.value())
            .style(SelectStyle::Group)
            .full_width()
            .on_change(move |value, _, cx| {
                let sort = Sort::from_value(value);
                sort_entity.update(cx, |this, cx| this.update_filters(cx, |filters| filters.sort = sort));
            }),
    );
    if collections.is_empty() {
        return order;
    }
    let options = std::iter::once((
        SharedString::default(),
        SharedString::from(t!("library.filters.collection.any")),
    ))
    .chain(collections.iter().map(|(id, name)| {
        let count = tallies.collections.get(id).copied().unwrap_or(0);
        (
            SharedString::from(id.clone()),
            SharedString::from(format!("{name} ({count})")),
        )
    }));
    let collection_entity = entity.clone();
    order
        .child(div().mt_4().child(group_title(&t!("library.filters.collection.title"))))
        .child(
            Select::new(
                "filter-collection",
                options,
                current.collection.clone().unwrap_or_default(),
            )
            .style(SelectStyle::Group)
            .full_width()
            .on_change(move |value, _, cx| {
                let value = value.to_string();
                collection_entity.update(cx, |this, cx| {
                    this.update_filters(cx, |filters| {
                        filters.collection = (!value.is_empty()).then_some(value);
                    });
                });
            }),
        )
}

fn account_group(
    entity: &Entity<SidebarView>,
    current: &Filters,
    accounts: Vec<(String, String)>,
    tallies: &Tallies,
) -> Div {
    filter_group(
        &t!("library.filters.account.title"),
        &t!("library.filters.account.note"),
    )
    .children(accounts.into_iter().map(|(id, name)| {
        let toggle_id = id.clone();
        check_row(
            entity,
            format!("check-account-{id}").into(),
            name,
            tallies.accounts.get(&id).copied().unwrap_or(0),
            current.accounts.contains(&id),
            move |filters| {
                if !filters.accounts.insert(toggle_id.clone()) {
                    filters.accounts.remove(&toggle_id);
                }
            },
        )
    }))
}

fn store_group(
    entity: &Entity<SidebarView>,
    current: &Filters,
    storefronts: Vec<Storefront>,
    tallies: &Tallies,
) -> Div {
    filter_group(
        &t!("library.filters.store.title"),
        &t!("library.filters.alternatives_note"),
    )
    .children(storefronts.into_iter().map(|storefront| {
        check_row(
            entity,
            format!("check-store-{}", storefront.as_str_name()).into(),
            storefront_label(storefront).into_owned(),
            tallies.storefronts.get(&storefront).copied().unwrap_or(0),
            current.storefronts.contains(&storefront),
            move |filters| {
                if !filters.storefronts.insert(storefront) {
                    filters.storefronts.remove(&storefront);
                }
            },
        )
    }))
}

fn genre_group(
    entity: &Entity<SidebarView>,
    current: &Filters,
    genres: Vec<String>,
    tallies: &Tallies,
    panel_width: f32,
) -> Div {
    // `repeat(auto-fill, minmax(160px, 1fr))`, worked out from the
    // panel's width: GPUI's grid only takes a fixed column count.
    let genre_columns = (((panel_width - 48. + 26.) / (160. + 26.)).floor() as u16).max(1);
    let genre_body = if genres.is_empty() {
        theme::muted(t!("library.filters.genre.none")).into_any_element()
    } else {
        div()
            .grid()
            .grid_cols(genre_columns)
            .gap_x(px(26.))
            .children(genres.into_iter().map(|name| {
                let toggle_name = name.clone();
                check_row(
                    entity,
                    format!("check-genre-{name}").into(),
                    name.clone(),
                    tallies.genres.get(&name).copied().unwrap_or(0),
                    current.genres.contains(&name),
                    move |filters| {
                        if !filters.genres.insert(toggle_name.clone()) {
                            filters.genres.remove(&toggle_name);
                        }
                    },
                )
            }))
            .into_any_element()
    };
    filter_group(
        &t!("library.filters.genre.title"),
        &t!("library.filters.alternatives_note"),
    )
    .col_span_full()
    .child(genre_body)
}

fn check_row(
    entity: &Entity<SidebarView>,
    id: SharedString,
    label: String,
    tally: usize,
    checked: bool,
    toggle: impl Fn(&mut Filters) + 'static,
) -> Stateful<Div> {
    let entity = entity.clone();
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(9.))
        .py_1()
        .text_size(px(12.5))
        .cursor_pointer()
        .hover(|row| row.text_color(rgb(theme::TEXT_BRIGHT)))
        .on_click(move |_, _, cx| entity.update(cx, |this, cx| this.update_filters(cx, |f| toggle(f))))
        .child(checkbox(checked))
        .child(div().min_w_0().truncate().child(label))
        .child(
            theme::mono()
                .ml_auto()
                .text_size(px(10.))
                .text_color(rgb(theme::TEXT_DIM))
                .child(tally.to_string()),
        )
}

fn filter_group(title: &str, note: &str) -> Div {
    div().flex().flex_col().child(group_title(title)).child(
        div()
            .mb_2()
            .text_size(px(10.5))
            .line_height(gpui::relative(1.45))
            .text_color(rgb(theme::TEXT_DIM))
            .child(note.to_owned()),
    )
}

fn group_title(text: &str) -> gpui::Div {
    theme::display(11.)
        .mb_1()
        .text_color(rgb(theme::ACCENT_BLUE))
        .child(text.to_uppercase())
}

/// `.link-btn`: bare muted text, blue on a panel fill when hovered.
fn link_button(id: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(5.))
        .py(px(3.))
        .rounded(px(theme::RADIUS_SM))
        .text_size(px(12.))
        .text_color(rgb(theme::TEXT_MUTED))
        .cursor_pointer()
        .hover(|button| button.text_color(rgb(theme::ACCENT_BLUE)).bg(rgb(theme::BG_PANEL)))
}
