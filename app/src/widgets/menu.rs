//! Floating layers: the right-click menus and popups (`.context-menu` over
//! a transparent `.context-backdrop`) and the install modal.
//!
//! Each layer brings its own full-window backdrop, same as the webview
//! build: a click outside closes the layer *and* goes no further, so
//! dismissing a menu over a game row doesn't also select that game.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Div, ElementId, FontWeight, MouseButton, Pixels, Point, SharedString, Stateful, Window, anchored,
    deferred, div, point, prelude::*, px, rgb, rgba,
};

use crate::theme;

pub type Dismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// A layer at `position` (window coordinates), kept 6px inside the window.
pub fn popup(position: Point<Pixels>, content: impl IntoElement, window: &Window, dismiss: Dismiss) -> AnyElement {
    let viewport = window.viewport_size();
    let dismiss_down = Rc::clone(&dismiss);
    deferred(
        anchored().position(point(px(0.), px(0.))).child(
            div()
                .id("popup-backdrop")
                .w(viewport.width)
                .h(viewport.height)
                .occlude()
                .on_mouse_down(MouseButton::Left, move |_, window, cx| dismiss_down(window, cx))
                .on_mouse_down(MouseButton::Right, move |_, window, cx| dismiss(window, cx))
                .child(
                    anchored().position(position).snap_to_window_with_margin(px(6.)).child(
                        div()
                            .occlude()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                            .child(content),
                    ),
                ),
        ),
    )
    .with_priority(1)
    .into_any_element()
}

/// `.modal-backdrop` + a centred panel; a click on the backdrop dismisses.
pub fn modal(content: impl IntoElement, window: &Window, dismiss: Dismiss) -> AnyElement {
    let viewport = window.viewport_size();
    deferred(
        anchored().position(point(px(0.), px(0.))).child(
            div()
                .id("modal-backdrop")
                .w(viewport.width)
                .h(viewport.height)
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x060c12b8))
                .occlude()
                .on_mouse_down(MouseButton::Left, move |_, window, cx| dismiss(window, cx))
                .child(
                    div()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(content),
                ),
        ),
    )
    .with_priority(2)
    .into_any_element()
}

/// `.context-menu`: the panel a menu's items sit in.
pub fn panel() -> Div {
    div()
        .flex()
        .flex_col()
        .min_w(px(210.))
        .max_w(px(280.))
        .p(px(6.))
        .bg(rgb(theme::BG_PANEL_2))
        .border_1()
        .border_color(rgb(theme::LINE))
        .rounded(px(theme::RADIUS))
        .shadow(theme::popup_shadow())
        .text_size(px(12.))
}

pub fn title(text: impl Into<SharedString>) -> Div {
    div()
        .px_2()
        .pt_1()
        .pb_2()
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(theme::TEXT_BRIGHT))
        .truncate()
        .child(text.into())
}

/// `.context-menu-item`. `disabled` dims it and leaves it without hover;
/// the caller leaves the click handler off.
pub fn item(id: impl Into<ElementId>, disabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(7.))
        .w_full()
        .px_2()
        .py(px(6.))
        .rounded(px(theme::RADIUS_SM))
        .text_size(px(12.))
        .text_color(rgb(theme::TEXT_PRIMARY))
        .when(disabled, |item| item.opacity(0.4).cursor_not_allowed())
        .when(!disabled, |item| {
            item.cursor_pointer().hover(|item| item.bg(rgb(theme::BG_PANEL_LIGHT)))
        })
}

/// `.context-menu-danger`.
pub fn danger_item(id: impl Into<ElementId>) -> Stateful<Div> {
    item(id, false)
        .text_color(rgb(theme::ERROR_TEXT))
        .hover(|item| item.bg(rgb(theme::DANGER_HOVER_BG)))
}

/// `.context-menu-check`: the 12px tick column.
pub fn check(on: bool) -> Div {
    div()
        .w(px(12.))
        .flex_none()
        .text_color(rgb(theme::ACCENT_BLUE))
        .when(on, |check| check.child("✓"))
}

pub fn separator() -> Div {
    div().h(px(1.)).my(px(6.)).bg(rgb(theme::LINE))
}

/// `.context-menu-label`: the small Oswald section label.
pub fn label(text: &str) -> Div {
    theme::display(9.5)
        .px_2()
        .pt(px(2.))
        .pb_1()
        .text_color(rgb(theme::TEXT_DIM))
        .child(text.to_uppercase())
}

pub fn empty(text: impl Into<SharedString>) -> Div {
    div()
        .px_2()
        .pt(px(2.))
        .pb(px(6.))
        .text_size(px(11.))
        .text_color(rgb(theme::TEXT_DIM))
        .child(text.into())
}
