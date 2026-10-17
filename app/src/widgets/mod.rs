//! The form controls and small pieces every screen draws: Decanter's
//! buttons, tabs, checkboxes, progress bars and tooltips, plus the text
//! field and dropdown GPUI doesn't ship.

pub mod input;
pub mod menu;
pub mod select;

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyView, App, Context, Div, ElementId, FontWeight, IntoElement, ParentElement as _,
    Render, SharedString, Stateful, Styled as _, Window, div, prelude::*, px, relative, rgb,
};
use rust_i18n::t;

pub use input::{InputEvent, TextInput};
pub use select::Select;

use crate::i18n::Language;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// The default `button`: blue fill, dark text.
    Primary,
    /// `.secondary`: panel fill with a line border.
    Secondary,
    /// `.icon-btn`: bare muted text that turns red on hover — every one of
    /// them, neutral actions included, same as the stylesheet.
    Icon,
    /// `.play-btn`: the big green JUGAR.
    Play,
    /// `.play-btn.secondary`: play-sized, secondary colours (INSTALAR).
    PlaySecondary,
}

/// A button. `disabled` draws it at 40% and the caller leaves the click
/// handler off — `.when(!disabled, |b| b.on_click(..))`.
pub fn button(
    id: impl Into<ElementId>,
    kind: ButtonKind,
    label: impl Into<SharedString>,
    disabled: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS))
        .font_weight(FontWeight::BOLD)
        .line_height(relative(1.5))
        .child(label.into());
    let base = match kind {
        ButtonKind::Primary => base
            .px_4()
            .py(px(9.))
            .bg(rgb(theme::ACCENT_BLUE))
            .text_color(rgb(theme::TEXT_ON_BLUE))
            .when(!disabled, |b| b.hover(|b| b.bg(rgb(theme::ACCENT_BLUE_HOVER)))),
        ButtonKind::Secondary => base
            .px_4()
            .py(px(9.))
            .bg(rgb(theme::BG_PANEL))
            .border_1()
            .border_color(rgb(theme::LINE))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .when(!disabled, |b| b.hover(|b| b.bg(rgb(theme::BG_PANEL_LIGHT)))),
        ButtonKind::Icon => base
            .px(px(10.))
            .py(px(6.))
            .text_color(rgb(theme::TEXT_MUTED))
            .when(!disabled, |b| b.hover(|b| b.text_color(rgb(theme::ERROR_TEXT)))),
        ButtonKind::Play => base
            .px(px(36.))
            .py(px(13.))
            .bg(rgb(theme::ACCENT_GREEN))
            .text_color(rgb(theme::TEXT_ON_GREEN))
            .when(!disabled, |b| b.hover(|b| b.bg(rgb(theme::ACCENT_GREEN_HOVER)))),
        ButtonKind::PlaySecondary => base
            .px(px(36.))
            .py(px(13.))
            .bg(rgb(theme::BG_PANEL))
            .border_1()
            .border_color(rgb(theme::LINE))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .when(!disabled, |b| b.hover(|b| b.bg(rgb(theme::BG_PANEL_LIGHT)))),
    };
    if disabled {
        base.opacity(0.4).cursor_not_allowed()
    } else {
        base.cursor_pointer().active(|b| b.mt(px(1.)).mb(px(-1.)))
    }
}

/// The text shown by a `title` attribute in the webview build.
pub struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .bg(rgb(theme::BG_PANEL_2))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .shadow(theme::popup_shadow())
            .text_size(px(12.))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .child(self.0.clone())
    }
}

/// Builder for `.tooltip(..)`: `div.tooltip(tooltip("Ajustes"))`.
pub fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tooltip(text.clone())).into()
}

/// A game's title in a sidebar row, green while the game runs, as Steam marks it. Shared by
/// Biblioteca and the PS section so a running game reads the same in both.
pub fn sidebar_title(title: impl Into<SharedString>, running: bool) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .truncate()
        .when(running, |title| title.text_color(rgb(theme::ACCENT_GREEN)))
        .child(title.into())
}

/// The mark at the end of a running game's sidebar row, in place of its installed/ready dot.
pub fn running_mark(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .text_size(px(10.))
        .text_color(rgb(theme::ACCENT_GREEN))
        .tooltip(tooltip(t!("common.running")))
        .child("▶")
}

/// A checkbox the way `accent-color: var(--accent-blue)` draws one: a
/// 13px box, blue and ticked when on.
pub fn checkbox(checked: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(13.))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .when(checked, |b| {
            b.bg(rgb(theme::ACCENT_BLUE))
                .border_color(rgb(theme::ACCENT_BLUE))
                .text_color(rgb(theme::TEXT_ON_BLUE))
                .text_size(px(10.))
                .font_weight(FontWeight::BOLD)
                .child("✓")
        })
        .when(!checked, |b| {
            b.bg(rgb(theme::BG_PANEL)).border_color(rgb(theme::TEXT_DIM))
        })
}

/// `.bar-track` + `.bar-fill`: determinate when `fraction` is known, the
/// sliding 30% block otherwise.
pub fn progress_bar(id: impl Into<ElementId>, fraction: Option<f64>, height: f32) -> Div {
    let track = div()
        .w_full()
        .h(px(height))
        .bg(rgb(theme::BG_PANEL_3))
        .rounded_full()
        .overflow_hidden()
        .relative();
    match fraction {
        Some(fraction) => track.child(
            div()
                .h_full()
                .w(relative(fraction as f32))
                .rounded_full()
                .bg(rgb(theme::ACCENT_BLUE)),
        ),
        None => track.child(
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(relative(0.3))
                .rounded_full()
                .bg(rgb(theme::ACCENT_BLUE))
                .with_animation(
                    id,
                    Animation::new(Duration::from_millis(1200))
                        .repeat()
                        .with_easing(gpui::ease_in_out),
                    // translateX(-100%) → translateX(333%) of a 30% block:
                    // its left edge goes from -30% to ~100% of the track.
                    |bar, delta| bar.left(relative(-0.3 + delta * 1.3)),
                ),
        ),
    }
}

/// `.tabs`: a row of uppercase labels, the active one underlined in blue.
pub fn tabs<T: Copy + PartialEq + 'static>(
    id: &'static str,
    items: &[(T, impl AsRef<str>)],
    active: T,
    on_select: impl Fn(T, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    div()
        .flex()
        .gap_6()
        .border_b_1()
        .border_color(rgb(theme::LINE))
        .children(items.iter().enumerate().map(|(index, (item, label))| {
            let tab = *item;
            let selected = tab == active;
            let on_select = on_select.clone();
            div()
                .id((id, index))
                .pb_3()
                .mb(px(-1.))
                .border_b_2()
                .border_color(if selected {
                    rgb(theme::ACCENT_BLUE).into()
                } else {
                    gpui::transparent_black()
                })
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(if selected {
                    theme::TEXT_BRIGHT
                } else {
                    theme::TEXT_MUTED
                }))
                .cursor_pointer()
                .hover(|tab| tab.text_color(rgb(theme::TEXT_BRIGHT)))
                .on_click(move |_, window, cx| on_select(tab, window, cx))
                .child(label.as_ref().to_uppercase())
        }))
}

/// `.stat-card`: a big mono number over a small uppercase label.
pub fn stat_card(value: impl Into<SharedString>, label: &str) -> Div {
    div()
        .flex_1()
        .min_w(px(140.))
        .bg(rgb(theme::BG_PANEL_2))
        .border_1()
        .border_color(rgb(theme::LINE))
        .rounded(px(theme::RADIUS))
        .px_4()
        .py(px(14.))
        .child(
            theme::mono()
                .text_size(px(17.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(theme::TEXT_BRIGHT))
                .child(value.into()),
        )
        .child(
            div()
                .mt_1()
                .text_size(px(10.5))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(label.to_uppercase()),
        )
}

#[derive(Clone, Copy)]
pub enum Emoji {
    Fire,
    Wastebasket,
}

static FIRE: LazyLock<Arc<gpui::Image>> = LazyLock::new(|| {
    Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        include_bytes!("../../assets/emoji/fire.png").to_vec(),
    ))
});
static WASTEBASKET: LazyLock<Arc<gpui::Image>> = LazyLock::new(|| {
    Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        include_bytes!("../../assets/emoji/wastebasket.png").to_vec(),
    ))
});

/// A colour emoji at a text size, drawn from a bundled bitmap: GPUI only
/// renders colour glyphs from Noto Color Emoji, and the system's emoji
/// font here (Twemoji, what the webview showed) comes out blank.
pub fn emoji(which: Emoji, text_size: f32) -> gpui::Img {
    let image = match which {
        Emoji::Fire => Arc::clone(&FIRE),
        Emoji::Wastebasket => Arc::clone(&WASTEBASKET),
    };
    // The bitmaps are 76x72; a glyph is about as tall as the font size.
    gpui::img(image).h(px(text_size)).w(px(text_size * 76. / 72.))
}

/// The UI language picker, shared by Ajustes and the first-run screen so
/// both offer the same choices. Native names, never translated, so it can be
/// used from a UI in a language the reader doesn't know.
pub fn language_select(
    selected: Option<Language>,
    on_change: impl Fn(Option<Language>, &mut Window, &mut App) + 'static,
) -> Select {
    const SYSTEM_LANGUAGE: &str = "system";
    let options = std::iter::once((SYSTEM_LANGUAGE.into(), t!("settings.general.language.system").into()))
        .chain(Language::ALL.map(|language| (language.code().into(), language.native_name().into())));
    Select::new("language", options, selected.map_or(SYSTEM_LANGUAGE, Language::code))
        .on_change(move |value, window, cx| on_change(Language::from_code(value), window, cx))
}
