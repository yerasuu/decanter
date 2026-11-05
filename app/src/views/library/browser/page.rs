//! One shelf opened as a page: every game of it in a wrapping grid, the way Steam enters a
//! collection. Rows of covers are laid out only while in view, like the shelves' rows are.

use std::collections::HashMap;

use gpui::{
    Context, FocusHandle, KeyDownEvent, ScrollHandle, SharedString, Window, div, point, prelude::*, px, relative, rgb,
};
use rust_i18n::t;

use super::shelf::{CARD_GAP, CARD_STRIDE, ROW_PADDING, auto_badge, cover_card};
use super::{BrowserView, Capsule, ShelfData};
use crate::theme;
use crate::views::library::context_menu::MenuTarget;
use crate::views::library::{LibraryPopup, SIDEBAR_WIDTH};
use crate::widgets::nav;

/// A cover's height plus the room under it, the same for every row so the rows in view follow from
/// the scroll offset alone.
const ROW_STRIDE: f32 = 250.;
const GRID_TOP: f32 = 18.;
const OVERSCAN_ROWS: isize = 2;

pub(super) struct Page {
    /// Key of the slot whose shelf this page shows.
    pub(super) slot: u32,
    /// Keeps Escape and the arrows reaching the page while no cover holds the focus.
    pub(super) focus: FocusHandle,
    scroll: ScrollHandle,
    /// Only the covers laid out keep a handle, as in a shelf's row.
    cards: HashMap<SharedString, FocusHandle>,
    shown: (usize, usize),
    columns: usize,
    revealed: Option<FocusHandle>,
}

impl Page {
    pub(super) fn new(slot: u32, focus: FocusHandle) -> Self {
        Self {
            slot,
            focus,
            scroll: ScrollHandle::new(),
            cards: HashMap::new(),
            shown: (0, 0),
            columns: 1,
            revealed: None,
        }
    }

    /// From the window rather than the grid's own bounds, which are unknown until it has been laid
    /// out once.
    fn columns_in(window: &Window) -> usize {
        let width = window.viewport_size().width.as_f32() - SIDEBAR_WIDTH - ROW_PADDING * 2.;
        (((width + CARD_GAP) / CARD_STRIDE).floor() as usize).max(1)
    }

    fn scrolled(&self) -> f32 {
        -self.scroll.offset().y.as_f32()
    }

    fn visible(&self, count: usize, window: &Window) -> (usize, usize) {
        let height = window.viewport_size().height.as_f32();
        let top = self.scrolled() - GRID_TOP;
        let first_row = ((top / ROW_STRIDE).floor() as isize - OVERSCAN_ROWS).max(0) as usize;
        let last_row = (((top + height) / ROW_STRIDE).ceil() as isize + OVERSCAN_ROWS).max(0) as usize;
        let first = (first_row * self.columns).min(count);
        (first, (last_row * self.columns).min(count).max(first))
    }

    fn first_in_view(&self, count: usize) -> usize {
        let row = ((self.scrolled() - GRID_TOP) / ROW_STRIDE).ceil().max(0.) as usize;
        (row * self.columns).min(count.saturating_sub(1))
    }

    fn focused(&self, capsules: &[Capsule], window: &Window) -> Option<usize> {
        let (first, last) = self.shown;
        capsules
            .get(first..last)?
            .iter()
            .position(|capsule| {
                self.cards
                    .get(&capsule.id)
                    .is_some_and(|handle| handle.is_focused(window))
            })
            .map(|index| first + index)
    }

    /// Scrolls just far enough to show the row of cover `index` whole.
    fn reveal(&self, index: usize) {
        let offset = self.scroll.offset();
        let height = self.scroll.bounds().size.height.as_f32();
        let top = (index / self.columns) as f32 * ROW_STRIDE + GRID_TOP;
        let bottom = top + ROW_STRIDE;
        let shown = self.scrolled();
        let target = if top < shown {
            top
        } else if bottom > shown + height {
            bottom - height
        } else {
            return;
        };
        let target = target.min(self.scroll.max_offset().y.as_f32()).max(0.);
        self.scroll.set_offset(point(offset.x, px(-target)));
    }
}

impl BrowserView {
    pub(super) fn open_page(&mut self, slot: u32, window: &mut Window, cx: &mut Context<Self>) {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.page = Some(Page::new(slot, focus));
        cx.notify();
    }

    /// Back to the shelves, with the focus on the entry the page was opened from.
    fn close_page(&mut self, cx: &mut Context<Self>) {
        self.return_to = self.page.take().map(|page| page.slot);
        cx.notify();
    }

    pub(super) fn layout_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(page), Some(derived)) = (&mut self.page, &self.derived) else {
            return;
        };
        let Some(shelf) = derived.shelves.iter().find(|shelf| shelf.key == page.slot) else {
            return;
        };
        // Escape and the arrows only reach the page through the focus, which a click on empty
        // ground or a closed popup can leave nowhere.
        if window.focused(cx).is_none() {
            window.focus(&page.focus, cx);
        }
        page.columns = Page::columns_in(window);
        page.revealed = page.focused(&shelf.capsules, window).and_then(|index| {
            let handle = page.cards.get(&shelf.capsules[index].id).cloned();
            if handle != page.revealed {
                page.reveal(index);
            }
            handle
        });
        page.shown = page.visible(shelf.capsules.len(), window);
        let shown = &shelf.capsules[page.shown.0..page.shown.1];
        page.cards
            .retain(|id, handle| handle.is_focused(window) || shown.iter().any(|capsule| capsule.id == *id));
        for capsule in shown {
            page.cards
                .entry(capsule.id.clone())
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    /// Arrows over the grid: sideways by cover, up and down by column. Past the first row and the
    /// last cover they're left to the window's own tab order.
    fn on_page_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if nav::is_escape(keystroke) {
            self.close_page(cx);
            cx.stop_propagation();
            return;
        }
        let (Some(page), Some(derived)) = (&mut self.page, &self.derived) else {
            return;
        };
        let Some(shelf) = derived.shelves.iter().find(|shelf| shelf.key == page.slot) else {
            return;
        };
        let capsules = &shelf.capsules;
        let count = capsules.len();
        let focused = page.focused(capsules, window);
        if nav::is_menu_key(keystroke) {
            let Some(index) = focused else {
                return;
            };
            let row = index / page.columns - page.shown.0 / page.columns;
            let Some(bounds) = page.scroll.bounds_for_item(row + usize::from(page.shown.0 > 0)) else {
                return;
            };
            let target = MenuTarget {
                product_ids: vec![capsules[index].id.to_string()],
                title: capsules[index].title.to_string(),
                position: point(
                    bounds.left() + px(((index % page.columns) as f32).mul_add(CARD_STRIDE, 24.)),
                    bounds.top() + px(48.),
                ),
            };
            self.model
                .update(cx, |model, cx| model.open_popup(LibraryPopup::GameMenu(target), cx));
            cx.stop_propagation();
            return;
        }
        if keystroke.modifiers.modified() {
            return;
        }
        let key = keystroke.key.as_str();
        let columns = page.columns;
        let target = match (key, focused) {
            ("left" | "right" | "up" | "down", None) => Some(page.first_in_view(count)),
            ("left", Some(index)) => index.checked_sub(1),
            ("right", Some(index)) => (index + 1 < count).then_some(index + 1),
            ("up", Some(index)) => index.checked_sub(columns),
            ("down", Some(index)) => {
                let below = index + columns;
                if below < count {
                    Some(below)
                } else {
                    (index / columns < (count - 1) / columns).then_some(count - 1)
                }
            }
            _ => return,
        };
        let Some(index) = target.filter(|_| count > 0) else {
            if matches!(key, "left" | "right") {
                cx.stop_propagation();
            }
            return;
        };
        let handle = page
            .cards
            .entry(capsules[index].id.clone())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        window.focus(&handle, cx);
        cx.stop_propagation();
    }

    pub(super) fn render_page(
        &self,
        page: &Page,
        shelf: &ShelfData,
        sources: &[(SharedString, SharedString)],
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let title = sources
            .iter()
            .find(|(id, _)| *id == shelf.source)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let count = shelf.capsules.len();
        let columns = page.columns;
        let (first, last) = page.shown;
        let (first_row, last_row) = (first / columns, last.div_ceil(columns));
        let spacer = |rows: usize| div().h(px(rows as f32 * ROW_STRIDE));
        let model = self.model.clone();

        div()
            .id("collection-page")
            .track_focus(&page.focus)
            .on_key_down(cx.listener(Self::on_page_key))
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(ROW_PADDING))
                    .pt(px(22.))
                    .pb(px(6.))
                    .child(
                        nav::tab_stop(div().id("collection-back"))
                            .flex_none()
                            .px(px(10.))
                            .py(px(5.))
                            .border_1()
                            .border_color(rgb(theme::LINE))
                            .rounded(px(theme::RADIUS_SM))
                            .text_size(px(12.))
                            .line_height(relative(1.))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .cursor_pointer()
                            .hover(|button| {
                                button
                                    .border_color(rgb(theme::ACCENT_BLUE))
                                    .text_color(rgb(theme::ACCENT_BLUE))
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.close_page(cx)))
                            .child(format!("‹ {}", t!("library.browser.back_to_shelves"))),
                    )
                    .child(theme::display(20.).text_color(rgb(theme::TEXT_BRIGHT)).child(title))
                    .when(shelf.dynamic, |head| head.child(auto_badge()))
                    .child(
                        theme::mono()
                            .flex_none()
                            .ml_auto()
                            .text_size(px(11.))
                            .text_color(rgb(theme::TEXT_DIM))
                            .child(t!(
                                "library.browser.installed_count",
                                installed = shelf.installed,
                                total = count
                            )),
                    ),
            )
            .child(
                div()
                    .id("collection-grid")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pt(px(GRID_TOP))
                    .px(px(ROW_PADDING))
                    .pb(px(56.))
                    .track_scroll(&page.scroll)
                    .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                    .when(first_row > 0, |grid| grid.child(spacer(first_row)))
                    .children((first_row..last_row).map(|row| {
                        let cards = row * columns..((row + 1) * columns).min(count);
                        div().flex().gap(px(CARD_GAP)).h(px(ROW_STRIDE)).children(
                            shelf.capsules[cards]
                                .iter()
                                .map(|capsule| cover_card(capsule, page.cards.get(&capsule.id), &model)),
                        )
                    }))
                    .when(last_row * columns < count, |grid| {
                        grid.child(spacer(count.div_ceil(columns) - last_row))
                    }),
            )
    }
}
