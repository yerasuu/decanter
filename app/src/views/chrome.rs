//! Window furniture for a window with no native title bar: the resize
//! strips, the minimize/maximize/close buttons, and the drag-to-move
//! surfaces. Shared by the main window and the properties window.

use std::rc::Rc;

use gpui::{
    App, CursorStyle, Div, MouseButton, ResizeEdge, Stateful, Window, WindowBounds, WindowDecorations, WindowOptions,
    div, prelude::*, px, rgb, size,
};

use crate::theme;
use crate::widgets::tooltip;

/// Options shared by every window: client-drawn decorations, since the
/// topbar draws its own controls, and the given minimum size.
pub fn window_options(
    title: &str,
    width: f32,
    height: f32,
    min_width: f32,
    min_height: f32,
    cx: &App,
) -> WindowOptions {
    let bounds = gpui::Bounds::centered(None, size(px(width), px(height)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(title.to_owned().into()),
            ..Default::default()
        }),
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(size(px(min_width), px(min_height))),
        app_id: Some("decanter".to_owned()),
        is_resizable: true,
        ..Default::default()
    }
}

/// Starts the move a native title bar would start on mousedown.
pub fn drag_handle(element: Div) -> Div {
    element.on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
}

/// The eight grab strips a native frame would have: 4px along each edge,
/// 8px in the corners (a corner is harder to hit), on top of everything
/// but too thin to take clicks from the content underneath.
pub fn resize_edges() -> impl Iterator<Item = Stateful<Div>> {
    const EDGES: [(&str, ResizeEdge); 8] = [
        ("resize-n", ResizeEdge::Top),
        ("resize-s", ResizeEdge::Bottom),
        ("resize-e", ResizeEdge::Right),
        ("resize-w", ResizeEdge::Left),
        ("resize-ne", ResizeEdge::TopRight),
        ("resize-nw", ResizeEdge::TopLeft),
        ("resize-se", ResizeEdge::BottomRight),
        ("resize-sw", ResizeEdge::BottomLeft),
    ];
    EDGES.into_iter().map(|(id, edge)| {
        let strip = div().id(id).absolute().occlude();
        let strip = match edge {
            ResizeEdge::Top => strip.top_0().left(px(8.)).right(px(8.)).h(px(4.)),
            ResizeEdge::Bottom => strip.bottom_0().left(px(8.)).right(px(8.)).h(px(4.)),
            ResizeEdge::Right => strip.right_0().top(px(8.)).bottom(px(8.)).w(px(4.)),
            ResizeEdge::Left => strip.left_0().top(px(8.)).bottom(px(8.)).w(px(4.)),
            ResizeEdge::TopRight => strip.top_0().right_0().size(px(8.)),
            ResizeEdge::TopLeft => strip.top_0().left_0().size(px(8.)),
            ResizeEdge::BottomRight => strip.bottom_0().right_0().size(px(8.)),
            ResizeEdge::BottomLeft => strip.bottom_0().left_0().size(px(8.)),
        };
        let cursor = match edge {
            ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
            ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
            ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
            ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
        };
        strip
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, move |_, window, _| window.start_window_resize(edge))
    })
}

/// Minimize/maximize/close. `on_close` is the caller's: the main window
/// has to stop every running game first, while a properties window is
/// just a second window with nothing of its own to shut down.
pub fn window_controls(on_close: Rc<dyn Fn(&mut Window, &mut App)>) -> Div {
    let window_button = |id: &'static str, glyph: &'static str, title: &'static str| {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .w(px(34.))
            .h(px(28.))
            .rounded(px(theme::RADIUS_SM))
            .text_size(px(11.))
            .text_color(rgb(theme::TEXT_MUTED))
            .cursor_pointer()
            .tooltip(tooltip(title))
            .child(glyph)
    };
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .child(
            window_button("window-minimize", "─", "Minimizar")
                .hover(|b| b.bg(rgb(theme::BG_PANEL_2)).text_color(rgb(theme::TEXT_PRIMARY)))
                .on_click(|_, window, _| window.minimize_window()),
        )
        .child(
            window_button("window-maximize", "☐", "Maximizar")
                .hover(|b| b.bg(rgb(theme::BG_PANEL_2)).text_color(rgb(theme::TEXT_PRIMARY)))
                .on_click(|_, window, _| window.zoom_window()),
        )
        .child(
            window_button("window-close", "✕", "Cerrar")
                .hover(|b| b.bg(rgb(theme::CLOSE_HOVER_BG)).text_color(rgb(theme::TEXT_BRIGHT)))
                .on_click(move |_, window, cx| on_close(window, cx)),
        )
}

/// `.logo`: the blue dot and the window's name, which doubles as a drag
/// handle the way a native title bar's text does.
pub fn logo(text: &str) -> Div {
    drag_handle(
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(7.))
            .font_family(theme::DISPLAY)
            .font_weight(gpui::FontWeight::BOLD)
            .text_size(px(16.))
            .text_color(rgb(theme::TEXT_BRIGHT))
            .child(div().size(px(8.)).rounded_full().bg(rgb(theme::ACCENT_BLUE)))
            .child(text.to_owned()),
    )
}
