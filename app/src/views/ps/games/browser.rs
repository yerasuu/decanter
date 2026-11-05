//! The covers of every dump, a shelf per platform, shown while no game is
//! selected — Biblioteca's shelf browser for PS games.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use decanter_ps::library::Platform;
use decanter_ps::status::Entry;
use gpui::{
    Bounds, BoxShadow, Context, Entity, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, ObjectFit, Pixels,
    ScrollHandle, SharedString, Subscription, WeakEntity, Window, div, img, linear_color_stop, linear_gradient, point,
    prelude::*, px, rgb,
};
use rust_i18n::t;

use super::GamesView;
use super::detail::DetailView;
use super::{art_url, display_title, entry_status_color, entry_status_text, platform_label};
use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, nav};

/// Square, like `icon0.png`.
const COVER: f32 = 156.;
const GROUP: &str = "ps-cover";

struct Card {
    focus: FocusHandle,
    /// Scrolls the cover into view whenever it gains the focus, by arrow or by Tab.
    _on_focus: Subscription,
}

pub struct BrowserView {
    ps: Entity<Ps>,
    detail: Entity<DetailView>,
    /// Holds the game menu a right-click on a cover opens.
    games: WeakEntity<GamesView>,
    scroll: ScrollHandle,
    /// Owned here rather than by each cover, so the arrows can move the focus to a cover that
    /// another row or shelf lays out.
    cards: HashMap<PathBuf, Card>,
    /// The dumps of each shelf in the order they are laid out.
    shelves: Vec<Vec<PathBuf>>,
    /// Where each cover was painted, to find the columns of a wrapped shelf and to scroll a
    /// focused cover into view.
    bounds: Rc<RefCell<HashMap<PathBuf, Bounds<Pixels>>>>,
    /// The cover a game was opened from with the keyboard, focused again once its page closes.
    opened_from: Option<FocusHandle>,
}

impl BrowserView {
    pub fn new(
        ps: Entity<Ps>,
        detail: Entity<DetailView>,
        games: WeakEntity<GamesView>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(&ps, |_, _, cx| cx.notify()).detach();
        Self {
            ps,
            detail,
            games,
            scroll: ScrollHandle::new(),
            cards: HashMap::new(),
            shelves: Vec::new(),
            bounds: Rc::default(),
            opened_from: None,
        }
    }

    pub const fn take_opened_from(&mut self) -> Option<FocusHandle> {
        self.opened_from.take()
    }

    /// Gives every dump a focus handle and lists each shelf's dumps in layout order.
    fn sync_cards(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shelves = [Platform::Ps4, Platform::Ps5]
            .into_iter()
            .map(|platform| {
                self.ps
                    .read(cx)
                    .entries()
                    .iter()
                    .filter(|entry| entry.game.platform == platform)
                    .map(|entry| entry.game.dir.clone())
                    .collect::<Vec<_>>()
            })
            .filter(|dirs| !dirs.is_empty())
            .collect();
        let shown: HashSet<&PathBuf> = self.shelves.iter().flatten().collect();
        self.cards.retain(|dir, _| shown.contains(dir));
        for dir in self.shelves.iter().flatten() {
            self.cards.entry(dir.clone()).or_insert_with(|| {
                let focus = cx.focus_handle().tab_stop(true);
                let revealed = dir.clone();
                let _on_focus = cx.on_focus(&focus, window, move |this, _, _| this.reveal(&revealed));
                Card { focus, _on_focus }
            });
        }
    }

    fn focused(&self, window: &Window) -> Option<(usize, usize)> {
        self.shelves.iter().enumerate().find_map(|(shelf, dirs)| {
            let index = dirs
                .iter()
                .position(|dir| self.cards.get(dir).is_some_and(|card| card.focus.is_focused(window)))?;
            Some((shelf, index))
        })
    }

    /// How many covers a shelf fits per row, read from where the first row was painted.
    fn columns(&self, dirs: &[PathBuf]) -> usize {
        let bounds = self.bounds.borrow();
        let Some(top) = dirs.first().and_then(|dir| bounds.get(dir)).map(Bounds::top) else {
            return 1;
        };
        dirs.iter()
            .take_while(|dir| bounds.get(*dir).is_some_and(|painted| painted.top() == top))
            .count()
            .max(1)
    }

    /// The cover above or below `index` in its column, in the next shelf once past the last row.
    fn vertical(&self, shelf: usize, index: usize, up: bool) -> Option<&PathBuf> {
        let dirs = &self.shelves[shelf];
        let columns = self.columns(dirs);
        let column = index % columns;
        if up {
            if index >= columns {
                return dirs.get(index - columns);
            }
            let above = self.shelves.get(shelf.checked_sub(1)?)?;
            let above_columns = self.columns(above);
            let last_row = above.len().saturating_sub(1) / above_columns * above_columns;
            return above.get((last_row + column).min(above.len().saturating_sub(1)));
        }
        if index + columns < dirs.len() {
            return dirs.get(index + columns);
        }
        if index / columns < dirs.len().saturating_sub(1) / columns {
            return dirs.last();
        }
        let below = self.shelves.get(shelf + 1)?;
        below.get(column.min(below.len().saturating_sub(1)))
    }

    /// Scrolls just far enough to show the cover of `dir` whole.
    fn reveal(&self, dir: &Path) {
        let Some(card) = self.bounds.borrow().get(dir).copied() else {
            return;
        };
        let view = self.scroll.bounds();
        let margin = px(24.);
        let shift = if card.top() < view.top() + margin {
            view.top() + margin - card.top()
        } else if card.bottom() > view.bottom() - margin {
            view.bottom() - margin - card.bottom()
        } else {
            return;
        };
        let offset = self.scroll.offset();
        let y = (offset.y + shift)
            .as_f32()
            .clamp(-self.scroll.max_offset().y.as_f32(), 0.);
        self.scroll.set_offset(point(offset.x, px(y)));
    }

    /// Up and down walk a shelf by column and cross to the next shelf, the menu key opens the
    /// focused game's menu; sideways falls to the window's tab order, which is the layout order.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((shelf, index)) = self.focused(window) else {
            return;
        };
        let keystroke = &event.keystroke;
        if nav::is_menu_key(keystroke) {
            let dir = self.shelves[shelf][index].clone();
            let Some(card) = self.bounds.borrow().get(&dir).copied() else {
                return;
            };
            let _ = self
                .games
                .update(cx, |games, cx| games.open_menu(dir, card.center(), window, cx));
            cx.stop_propagation();
            return;
        }
        if keystroke.modifiers.modified() {
            return;
        }
        let up = match keystroke.key.as_str() {
            "up" => true,
            "down" => false,
            _ => return,
        };
        if let Some(handle) = self.vertical(shelf, index, up).and_then(|dir| self.cards.get(dir)) {
            window.focus(&handle.focus, cx);
            cx.stop_propagation();
        }
    }

    fn card(&self, entry: &Entry, cx: &Context<Self>) -> impl IntoElement + use<> {
        let compat = self.ps.read(cx).compat();
        let game = &entry.game;
        let dir = game.dir.clone();
        let focus = self.cards.get(&dir).map(|card| &card.focus);
        let (games, menu_dir) = (self.games.clone(), game.dir.clone());
        let art = div()
            .id("ps-cover-art")
            .relative()
            .size(px(COVER))
            .rounded(px(theme::RADIUS))
            .overflow_hidden()
            .bg(rgb(theme::BG_PANEL_2))
            .group_hover(GROUP, |art| {
                art.top(px(-4.)).shadow(vec![
                    BoxShadow {
                        color: rgb(theme::ACCENT_BLUE).into(),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(0.),
                        spread_radius: px(2.),
                        inset: false,
                    },
                    BoxShadow {
                        color: gpui::black().opacity(0.45),
                        offset: point(px(0.), px(10.)),
                        blur_radius: px(22.),
                        spread_radius: px(0.),
                        inset: false,
                    },
                ])
            })
            .group_active(GROUP, |art| art.top(px(-1.)))
            .child(
                img(crate::art::source(
                    &art_url(game, "icon0.png"),
                    gpui::size(COVER, COVER),
                ))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .object_fit(ObjectFit::Cover)
                .with_fallback(|| div().into_any_element()),
            )
            // Only the bottom edge darkens, for the status pill; the art
            // itself already carries the game's name.
            .child(div().absolute().top_0().left_0().size_full().bg(linear_gradient(
                180.,
                linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 0.6),
                linear_color_stop(theme::alpha(theme::BG_DEEP, 0.8), 1.),
            )))
            .child(
                div()
                    .absolute()
                    .bottom(px(8.))
                    .left(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .px(px(6.))
                    .py(px(1.))
                    .rounded(px(theme::RADIUS_SM))
                    .bg(theme::alpha(theme::BG_DEEP, 0.85))
                    .text_size(px(10.))
                    .text_color(rgb(entry_status_color(entry, compat)))
                    .child(SharedString::from(entry_status_text(entry, compat))),
            );
        nav::focus_ring(div().id(SharedString::from(format!("ps-cover-{}", game.dir.display()))))
            .when_some(focus, InteractiveElement::track_focus)
            .group(GROUP)
            .w(px(COVER))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(theme::RADIUS))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.opened_from = window.last_input_was_keyboard().then(|| window.focused(cx)).flatten();
                this.detail.update(cx, |detail, cx| detail.select(dir.clone(), cx));
            }))
            .on_mouse_down(MouseButton::Right, move |event: &MouseDownEvent, window, cx| {
                let _ = games.update(cx, |games, cx| {
                    games.open_menu(menu_dir.clone(), event.position, window, cx)
                });
            })
            .child(art)
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(rgb(theme::TEXT_PRIMARY))
                    .line_clamp(2)
                    .child(display_title(game)),
            )
    }
}

impl Render for BrowserView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_cards(window, cx);
        let ps = self.ps.read(cx);
        let shelves: Vec<_> = [Platform::Ps4, Platform::Ps5]
            .into_iter()
            .filter_map(|platform| {
                let (dirs, cards): (Vec<_>, Vec<_>) = ps
                    .entries()
                    .iter()
                    .filter(|entry| entry.game.platform == platform)
                    .map(|entry| (entry.game.dir.clone(), self.card(entry, cx).into_any_element()))
                    .unzip();
                (!cards.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(14.))
                        .child(theme::display(15.).text_color(rgb(theme::TEXT_MUTED)).child(format!(
                            "{} · {}",
                            platform_label(platform),
                            cards.len()
                        )))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(px(18.))
                                .on_children_prepainted({
                                    let (bounds, dirs) = (Rc::clone(&self.bounds), dirs);
                                    move |painted, _, _| {
                                        bounds.borrow_mut().extend(dirs.iter().cloned().zip(painted));
                                    }
                                })
                                .children(cards),
                        )
                })
            })
            .collect();
        let empty = shelves.is_empty();
        div()
            .id("ps-browser")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .p(px(32.))
                    .flex()
                    .flex_col()
                    .gap(px(32.))
                    .when(empty, |page| {
                        page.child(
                            theme::empty(t!("ps.games.browser_empty"))
                                .flex()
                                .flex_col()
                                .items_start()
                                .gap_3()
                                .child(crate::views::ps::adding::add_button(
                                    "ps-add-browser",
                                    ButtonKind::Primary,
                                    &self.ps,
                                    cx,
                                )),
                        )
                    })
                    .children(shelves),
            )
    }
}
