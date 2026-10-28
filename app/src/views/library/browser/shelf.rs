use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, BoxShadow, Context, Div, Entity, FontWeight, MouseButton, ObjectFit, ScrollHandle,
    SharedString, Stateful, Window, div, img, linear_color_stop, linear_gradient, point, prelude::*, px, relative, rgb,
};
use rust_i18n::t;

use super::{BrowserView, Capsule, ShelfData};
use crate::views::library::context_menu::MenuTarget;
use crate::views::library::filters::Sort;
use crate::views::library::{LibraryModel, LibraryPopup};
use crate::widgets::select::SelectStyle;
use crate::widgets::{Select, tooltip};
use crate::{art, theme};

/// `rgba(10, 22, 32, .88)`, the dark chip behind a capsule's pips.
const PIP_BG: u32 = 0x0a1620;

const CARD_GROUP: &str = "cover-card";
const HEAD_GROUP: &str = "browser-shelf-head";

/// A capsule's width plus the row's gap: every card takes the same room,
/// which is what lets a row lay out only the ones in view.
const CARD_GAP: f32 = 13.;
const CARD_STRIDE: f32 = 136. + CARD_GAP;
const ROW_PADDING: f32 = 26.;

/// What a shelf header carries while it's dragged to a new place.
struct ShelfDrag {
    key: u32,
}

/// The header's ghost under the cursor, standing in for the webview's
/// drag image of the header bar.
struct ShelfGhost {
    label: SharedString,
}

impl Render for ShelfGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(26.)).opacity(0.6).child(
            theme::display(15.)
                .text_color(rgb(theme::TEXT_BRIGHT))
                .child(self.label.to_uppercase()),
        )
    }
}

impl BrowserView {
    pub(super) fn render_shelf(
        &self,
        shelf: &ShelfData,
        sources: &[(SharedString, SharedString)],
        dragging: Option<u32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let key = shelf.key;
        let head = self.shelf_head(shelf, sources, cx);
        let body = if shelf.capsules.is_empty() {
            empty_shelf().into_any_element()
        } else {
            self.render_row(shelf, cx).into_any_element()
        };

        div()
            .mb(px(30.))
            .when(dragging == Some(key), |shelf| shelf.opacity(0.45))
            .child(head)
            .child(body)
    }

    fn shelf_head(
        &self,
        shelf: &ShelfData,
        sources: &[(SharedString, SharedString)],
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let key = shelf.key;
        let only_slot = self.slots.len() == 1;
        let total = shelf.capsules.len();
        let ghost_label = sources
            .iter()
            .find(|(id, _)| *id == shelf.source)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let view = cx.entity();

        div()
            .id(("shelf-head", key))
            .group(HEAD_GROUP)
            .flex()
            .items_center()
            .gap(px(9.))
            .px(px(26.))
            .pb(px(11.))
            // Steam reorders shelves by dragging the header itself — the
            // whole bar is the handle.
            .cursor_grab()
            .on_drag(ShelfDrag { key }, move |drag, _, _, cx| {
                view.update(cx, |this, cx| {
                    this.dragging = Some(drag.key);
                    cx.notify();
                });
                let label = ghost_label.clone();
                cx.new(|_| ShelfGhost { label })
            })
            .drag_over::<ShelfDrag>(move |style, drag, _, _| {
                if drag.key == key {
                    return style;
                }
                style.shadow(vec![BoxShadow {
                    color: rgb(theme::ACCENT_BLUE).into(),
                    offset: point(px(0.), px(-2.)),
                    blur_radius: px(0.),
                    spread_radius: px(0.),
                    inset: false,
                }])
            })
            .on_drop(cx.listener(move |this, drag: &ShelfDrag, _, cx| this.reorder(drag.key, key, cx)))
            .child(
                div()
                    .id(("shelf-grip", key))
                    .flex_none()
                    .text_size(px(13.))
                    .line_height(relative(1.))
                    .text_color(rgb(theme::TEXT_DIM))
                    .group_hover(HEAD_GROUP, |grip| grip.text_color(rgb(theme::TEXT_MUTED)))
                    .tooltip(tooltip(t!("library.browser.drag_to_reorder")))
                    .child("⠿"),
            )
            // The source picker *is* the shelf's title.
            .child(
                Select::new(("shelf-source", key), sources.iter().cloned(), shelf.source.clone())
                    .style(SelectStyle::Heading)
                    .on_change(cx.listener(move |this, value: &SharedString, _, cx| {
                        this.update_slot(key, |slot| slot.source = value.to_string(), cx);
                    })),
            )
            .when(shelf.dynamic, |head| head.child(auto_badge()))
            .child(
                Select::new(("shelf-sort", key), Sort::options(), shelf.sort.value())
                    .style(SelectStyle::Small)
                    .on_change(cx.listener(move |this, value: &SharedString, _, cx| {
                        this.update_slot(key, |slot| slot.sort = Sort::from_value(value), cx);
                    })),
            )
            .child(
                theme::mono()
                    .flex_none()
                    .ml_auto()
                    .text_size(px(11.))
                    .text_color(rgb(theme::TEXT_DIM))
                    .child(t!(
                        "library.browser.installed_count",
                        installed = shelf.installed,
                        total = total
                    )),
            )
            .child(Self::remove_shelf_button(key, only_slot, cx))
    }

    fn remove_shelf_button(key: u32, only_slot: bool, cx: &Context<Self>) -> Stateful<Div> {
        div()
            .id(("shelf-remove", key))
            .flex_none()
            .px(px(8.))
            .py(px(5.))
            .border_1()
            .border_color(gpui::transparent_black())
            .rounded(px(theme::RADIUS_SM))
            .text_size(px(12.))
            .line_height(relative(1.))
            .text_color(rgb(theme::TEXT_DIM))
            .tooltip(tooltip(t!("library.browser.remove_shelf")))
            .when(only_slot, |button| button.opacity(0.3).cursor_not_allowed())
            .when(!only_slot, |button| {
                button
                    .cursor_pointer()
                    .hover(|button| {
                        button
                            .text_color(rgb(theme::ERROR_TEXT))
                            .bg(rgb(theme::BG_PANEL_2))
                            .border_color(rgb(theme::ERROR_LINE))
                    })
                    .active(|button| button.mt(px(1.)).mb(px(-1.)))
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_slot(key, cx)))
            })
            .child(crate::widgets::emoji(crate::widgets::Emoji::Wastebasket, 12.))
    }

    /// One shelf's scrolling row of capsules.
    ///
    /// One row that scrolls sideways rather than a wrapping grid: it keeps
    /// each shelf to a predictable height so several are visible at once.
    /// GPUI draws no scrollbar, which is what Steam does here too — the row
    /// is paged by the edge arrows (or a sideways wheel) instead.
    fn render_row(&self, shelf: &ShelfData, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let key = shelf.key;
        let (scroll, hovered) = match self.rows.get(&key) {
            Some(row) => (row.scroll.clone(), row.hovered),
            None => (ScrollHandle::new(), false),
        };
        // A pixel of slack either way: an eased scroll lands a hair off the
        // exact end often enough that comparing outright leaves the arrow
        // lit. A row not laid out yet reports a zero max offset, so it reads
        // as at both ends and neither arrow lights up on a shelf that fits.
        let offset = scroll.offset().x;
        let at_start = offset >= px(-1.);
        let at_end = -offset >= scroll.max_offset().x - px(1.);

        // Only the capsules in view (and a few either side) are laid out:
        // a "Toda la biblioteca" shelf is hundreds of covers, and laying
        // all of them out on every hover or progress tick was the slowdown.
        // Spacers stand in for the rest so the scroll range stays whole.
        let count = shelf.capsules.len();
        let (first, last) = visible_cards(count, scroll.bounds().size.width.as_f32(), -offset.as_f32());
        let spacer = |cards: usize| div().flex_none().w(px(cards as f32 * CARD_STRIDE - CARD_GAP));

        let model = self.model.clone();
        div()
            .id(("browser-viewport", key))
            .relative()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if let Some(row) = this.rows.get_mut(&key) {
                    row.hovered = *hovered;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .id(("browser-row", key))
                    .flex()
                    .gap(px(13.))
                    .pt(px(4.))
                    .px(px(26.))
                    .pb(px(10.))
                    .overflow_x_scroll()
                    // Otherwise a vertical wheel over a row pages it sideways
                    // instead of scrolling the browser past it.
                    .restrict_scroll_to_axis()
                    .track_scroll(&scroll)
                    // Windowing below depends on the offset, so a scroll
                    // has to re-render the row, not just repaint it.
                    .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                    .when(first > 0, |row| row.child(spacer(first)))
                    .children(
                        shelf.capsules[first..last]
                            .iter()
                            .map(|capsule| cover_card(capsule, &model)),
                    )
                    .when(last < count, |row| row.child(spacer(count - last))),
            )
            // An arrow with nothing left behind it is taken out entirely
            // rather than lighting up on hover and doing nothing.
            .when(hovered && !at_start, |viewport| {
                viewport.child(Self::row_arrow(key, true, cx))
            })
            .when(hovered && !at_end, |viewport| {
                viewport.child(Self::row_arrow(key, false, cx))
            })
    }

    fn row_arrow(key: u32, left: bool, cx: &Context<Self>) -> impl IntoElement + use<> {
        let (id, label, glyph, direction) = if left {
            ("row-arrow-left", t!("library.browser.previous"), "‹", -1.)
        } else {
            ("row-arrow-right", t!("library.browser.next"), "›", 1.)
        };
        let fade = theme::alpha(theme::BG_DEEP, 0.);
        div()
            .id((id, key))
            .absolute()
            .top_0()
            .bottom(px(18.))
            .w(px(46.))
            .when(left, |arrow| arrow.left_0())
            .when(!left, |arrow| arrow.right_0())
            .flex()
            .items_center()
            .justify_center()
            .bg(linear_gradient(
                if left { 90. } else { 270. },
                linear_color_stop(rgb(theme::BG_DEEP), 0.34),
                linear_color_stop(fade, 1.),
            ))
            .text_size(px(20.))
            .line_height(relative(1.))
            .text_color(rgb(theme::TEXT_BRIGHT))
            .cursor_pointer()
            .hover(|arrow| arrow.text_color(rgb(theme::ACCENT_BLUE)))
            .active(|arrow| arrow.top(px(1.)).bottom(px(17.)))
            // A click on the arrow mustn't also open the capsule behind
            // it. Not `block_mouse_except_scroll`: that also takes hover
            // away from the viewport this arrow lives in, which removed
            // the arrow the moment the pointer reached it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .tooltip(tooltip(label))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.page_row(key, direction, cx);
            }))
            .child(glyph)
            .with_animation(
                (id, key),
                Animation::new(Duration::from_millis(140)).with_easing(gpui::ease_in_out),
                |arrow, delta| arrow.opacity(delta),
            )
    }
}

fn empty_shelf() -> Div {
    div()
        .mx(px(26.))
        .p(px(16.))
        .border_1()
        .border_dashed()
        .border_color(rgb(theme::LINE))
        .rounded(px(theme::RADIUS))
        .text_size(px(12.))
        .text_color(rgb(theme::TEXT_DIM))
        .child(t!("library.browser.empty_shelf"))
}

fn auto_badge() -> Div {
    div()
        .flex_none()
        .text_size(px(8.5))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(theme::ACCENT_BLUE))
        .border_1()
        .border_color(rgb(theme::ACCENT_BLUE_DIM))
        .rounded(px(8.))
        .px(px(4.))
        .child(t!("library.shelves.auto"))
}

fn visible_cards(count: usize, viewport: f32, scrolled: f32) -> (usize, usize) {
    // Not laid out yet on the first frame; a wide guess beats a blank row.
    let viewport = if viewport > 0. { viewport } else { 2560. };
    // A full page laid out past either edge: a page arrow moves the row
    // by most of its width, and with any less the capsules it brought
    // in arrived still waiting on their art.
    let overscan = (viewport / CARD_STRIDE).ceil() as isize;
    let first = (((scrolled - ROW_PADDING) / CARD_STRIDE).floor() as isize - overscan).max(0) as usize;
    let last = ((((scrolled + viewport) / CARD_STRIDE).ceil() as isize + overscan).max(0) as usize).min(count);
    (first.min(last), last)
}

fn cover_card(capsule: &Capsule, model: &Entity<LibraryModel>) -> impl IntoElement {
    let click_model = model.clone();
    let menu_model = model.clone();
    let click_id = capsule.id.clone();
    let menu_id = capsule.id.clone();
    let menu_title = capsule.title.clone();
    div()
        .id(capsule.id.clone())
        .group(CARD_GROUP)
        .relative()
        .flex_none()
        .w(px(136.))
        .cursor_pointer()
        .tooltip(tooltip(capsule.title.clone()))
        .on_click(move |_, _, cx| {
            let id = click_id.to_string();
            click_model.update(cx, |model, cx| model.select(Some(id), cx));
        })
        .on_mouse_down(MouseButton::Right, move |event, _, cx| {
            cx.stop_propagation();
            let target = MenuTarget {
                product_ids: vec![menu_id.to_string()],
                title: menu_title.to_string(),
                position: event.position,
            };
            menu_model.update(cx, |model, cx| model.open_popup(LibraryPopup::GameMenu(target), cx));
        })
        .child(cover_art(capsule))
        .child(cover_pips(capsule))
        .child(
            div()
                .mt(px(7.))
                .text_size(px(11.))
                .text_color(rgb(theme::TEXT_MUTED))
                .truncate()
                .group_hover(CARD_GROUP, |meta| meta.text_color(rgb(theme::TEXT_PRIMARY)))
                .child(capsule.meta.clone()),
        )
}

fn cover_art(capsule: &Capsule) -> Stateful<Div> {
    // Art is missing often enough that the title has to be legible without
    // it, so it's drawn over the art in both cases and the fallback only
    // changes what's behind it.
    div()
        .id("cover-art")
        .relative()
        .w(px(136.))
        .h(px(204.))
        .rounded(px(theme::RADIUS))
        .flex()
        .items_end()
        .overflow_hidden()
        .bg(rgb(theme::BG_PANEL))
        .when(capsule.cover.is_none(), |art| {
            art.bg(rgb(theme::BG_PANEL_2)).shadow(vec![BoxShadow {
                color: rgb(theme::LINE).into(),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(1.),
                inset: true,
            }])
        })
        .group_hover(CARD_GROUP, |art| {
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
        .group_active(CARD_GROUP, |art| art.top(px(-1.)))
        .when_some(capsule.cover.as_deref(), |art, cover| {
            art.child(
                img(art::source(cover, gpui::size(136., 204.)))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .rounded(px(theme::RADIUS))
                    .object_fit(ObjectFit::Cover)
                    .with_fallback(|| div().into_any_element()),
            )
        })
        .child(div().absolute().top_0().left_0().size_full().bg(linear_gradient(
            180.,
            linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 0.42),
            linear_color_stop(theme::alpha(theme::BG_DEEP, 0.86), 1.),
        )))
        .child(
            // Three lines is what the scrim covers; a longer title would
            // climb out of the dark and back onto the artwork.
            theme::display(14.)
                .relative()
                .w_full()
                .p(px(11.))
                .line_height(relative(1.15))
                .text_color(rgb(theme::TEXT_BRIGHT))
                .line_clamp(3)
                .child(capsule.title.clone()),
        )
}

fn cover_pips(capsule: &Capsule) -> Div {
    div()
        .absolute()
        .top(px(7.))
        .right(px(7.))
        .flex()
        .gap(px(4.))
        .when(capsule.installed, |pips| {
            pips.child(pip(
                "cover-installed",
                "▶",
                theme::ACCENT_GREEN,
                t!("library.sidebar.installed").into(),
            ))
        })
        .when(capsule.favorite, |pips| {
            pips.child(pip(
                "cover-favorite",
                "★",
                theme::ACCENT_GOLD,
                t!("library.browser.favorite").into(),
            ))
        })
}

fn pip(id: &'static str, glyph: &'static str, color: u32, label: SharedString) -> Stateful<Div> {
    div()
        .id(id)
        .bg(theme::alpha(PIP_BG, 0.88))
        .rounded(px(theme::RADIUS_SM))
        .px(px(5.))
        .py(px(1.))
        .text_size(px(10.))
        .line_height(relative(1.45))
        .text_color(rgb(color))
        .tooltip(tooltip(label))
        .child(glyph)
}
