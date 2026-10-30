//! Window furniture for a window with no native title bar: the resize
//! strips, the minimize/maximize/close buttons, and the drag-to-move
//! surfaces. Shared by the main window and the properties window.

use std::rc::Rc;
use std::sync::{Arc, LazyLock};

use gpui::{
    App, CursorStyle, Div, Image, ImageFormat, MouseButton, ObjectFit, ResizeEdge, SharedString, Stateful, Window,
    WindowBounds, WindowControlArea, WindowDecorations, WindowOptions, div, img, prelude::*, px, rgb, size,
};
use rust_i18n::t;

use crate::platform::NATIVE_FRAME_CONTROLS;
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
    crate::platform::with_window_icon(WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(title.to_owned().into()),
            appears_transparent: NATIVE_FRAME_CONTROLS,
            ..Default::default()
        }),
        window_decorations: Some(WindowDecorations::Client),
        window_min_size: Some(size(px(min_width), px(min_height))),
        app_id: Some("decanter".to_owned()),
        is_resizable: true,
        ..Default::default()
    })
}

/// Starts the move a native title bar would start on mousedown.
pub fn drag_handle(element: Div) -> Div {
    element
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
}

/// The eight grab strips a native frame would have: 4px along each edge,
/// 8px in the corners (a corner is harder to hit), on top of everything
/// but too thin to take clicks from the content underneath. None where
/// the OS frame keeps its own borders.
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
    let edges: &[_] = if NATIVE_FRAME_CONTROLS { &[] } else { &EDGES };
    edges.iter().map(|&(id, edge)| {
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
    let window_button = |id: &'static str, glyph: &'static str, title: SharedString| {
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
            window_button("window-minimize", "─", t!("chrome.window.minimize").into())
                .hover(|b| b.bg(rgb(theme::BG_PANEL_2)).text_color(rgb(theme::TEXT_PRIMARY)))
                .window_control_area(WindowControlArea::Min)
                .on_click(|_, window, _| {
                    if !NATIVE_FRAME_CONTROLS {
                        window.minimize_window();
                    }
                }),
        )
        .child(
            window_button("window-maximize", "☐", t!("chrome.window.maximize").into())
                .hover(|b| b.bg(rgb(theme::BG_PANEL_2)).text_color(rgb(theme::TEXT_PRIMARY)))
                .window_control_area(WindowControlArea::Max)
                .on_click(|_, window, _| {
                    if !NATIVE_FRAME_CONTROLS {
                        window.zoom_window();
                    }
                }),
        )
        // Not marked `Close`, since Windows would then post its own close and
        // skip `on_close`, which has to stop the running games first.
        .child(
            window_button("window-close", "✕", t!("chrome.window.close").into())
                .hover(|b| b.bg(rgb(theme::CLOSE_HOVER_BG)).text_color(rgb(theme::TEXT_BRIGHT)))
                .on_click(move |_, window, cx| on_close(window, cx)),
        )
}

const LOGO_SIZE: f32 = 28.;

/// GPUI rasterises an SVG at its own `width`/`height` times two and the GPU
/// then samples that down with no mipmaps, so the 512 px artwork would shimmer
/// at 28 px. Declaring the displayed size makes the raster 2:1 at 1x and 1:1 at 2x.
static LOGO: LazyLock<Arc<Image>> = LazyLock::new(|| {
    let side = format!("width=\"{LOGO_SIZE}\" height=\"{LOGO_SIZE}\"");
    let svg = include_str!("../../assets/decanter.svg").replacen("width=\"512\" height=\"512\"", &side, 1);
    Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
});

/// `.logo`: the app's artwork and the window's name, which doubles as a drag
/// handle the way a native title bar's text does.
pub fn logo(text: &str) -> Div {
    drag_handle(
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(9.))
            .font_family(theme::DISPLAY)
            .font_weight(gpui::FontWeight::BOLD)
            .text_size(px(16.))
            .text_color(rgb(theme::TEXT_BRIGHT))
            .child(
                img(LOGO.clone())
                    .size(px(LOGO_SIZE))
                    .flex_none()
                    .object_fit(ObjectFit::Contain),
            )
            .child(text.to_owned()),
    )
}
