//! Decanter's look (`ui/assets/styling/main.css`), natively: the `:root`
//! tokens, the bundled fonts, and the handful of primitives (`.card`,
//! `.empty`, `.error`, `.badge`, headings) every screen shares.
//!
//! GPUI has no letter-spacing, so the CSS `letter-spacing` values are the
//! one part of the type scale that doesn't carry over.

use std::borrow::Cow;

use gpui::{App, BoxShadow, Div, FontFallbacks, FontWeight, Hsla, SharedString, div, point, prelude::*, px, rgb};

pub const BG_DEEP: u32 = 0x0e141b;
pub const BG_PANEL: u32 = 0x1b2838;
pub const BG_PANEL_2: u32 = 0x16202c;
pub const BG_PANEL_3: u32 = 0x0f1720;
pub const BG_PANEL_LIGHT: u32 = 0x2a3f5a;
pub const LINE: u32 = 0x2a3947;
pub const ACCENT_BLUE: u32 = 0x66c0f4;
pub const ACCENT_BLUE_DIM: u32 = 0x4a7a96;
pub const ACCENT_GREEN: u32 = 0xa4d007;
pub const ACCENT_GOLD: u32 = 0xe0b03e;
pub const TEXT_PRIMARY: u32 = 0xc6d4df;
pub const TEXT_MUTED: u32 = 0x8f98a0;
pub const TEXT_DIM: u32 = 0x5c6b76;
pub const TEXT_BRIGHT: u32 = 0xffffff;

pub const ERROR_TEXT: u32 = 0xf2a3a3;
pub const ERROR_BG: u32 = 0x2a1616;
pub const ERROR_LINE: u32 = 0x4a2323;
pub const DANGER_HOVER_BG: u32 = 0x3a2020;
pub const CLOSE_HOVER_BG: u32 = 0xc0392b;
pub const TEXT_ON_BLUE: u32 = 0x0a1620;
pub const TEXT_ON_GREEN: u32 = 0x132406;
/// `filter: brightness(1.12)` on the blue and green fills, precomputed:
/// GPUI has no filters, only the colour itself.
pub const ACCENT_BLUE_HOVER: u32 = 0x72d7ff;
pub const ACCENT_GREEN_HOVER: u32 = 0xb8e908;

pub const RADIUS: f32 = 6.;
pub const RADIUS_SM: f32 = 4.;
/// Floating panels (menus, dropdowns, dialogs): less their 6px padding it
/// leaves `RADIUS_SM`, so the items inside them sit concentric.
pub const RADIUS_LG: f32 = 10.;

pub const SANS: &str = "Inter";
pub const DISPLAY: &str = "Oswald";
pub const MONO: &str = "JetBrains Mono";

/// Bundled so the look doesn't depend on what the system has installed —
/// the webview build pulled these from Google Fonts on every launch.
pub fn load_fonts(cx: &App) -> anyhow::Result<()> {
    cx.text_system().add_fonts(vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/Inter-Medium.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/Inter-SemiBold.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/Oswald.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf").as_slice()),
    ])
}

pub fn alpha(color: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(color).into();
    color.a = alpha;
    color
}

/// `0 8px 24px rgba(0,0,0,.45)`, the popup/menu shadow.
pub fn popup_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: gpui::black().opacity(0.45),
        offset: point(px(0.), px(8.)),
        blur_radius: px(24.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// `h1`: Oswald 600, uppercase, 30px, bright, 16px under it.
pub fn h1(text: impl AsRef<str>) -> Div {
    display(30.)
        .text_color(rgb(TEXT_BRIGHT))
        .mb_4()
        .child(text.as_ref().to_uppercase())
}

/// `h2`: smaller and muted, with 32px above — the section heading inside a page.
pub fn h2(text: impl AsRef<str>) -> Div {
    display(15.)
        .text_color(rgb(TEXT_MUTED))
        .mt_8()
        .mb_4()
        .child(text.as_ref().to_uppercase())
}

/// `h3`: tailwind's preflight leaves it at the inherited 14px; still Oswald,
/// uppercase and bright from the shared heading rule.
pub fn h3(text: impl AsRef<str>) -> Div {
    display(14.)
        .text_color(rgb(TEXT_BRIGHT))
        .mb_4()
        .child(text.as_ref().to_uppercase())
}

/// The system fonts to draw a language's CJK text with, ahead of whatever the
/// platform picks on its own: Han characters are shared by zh, ja and ko, and
/// the platform's pick follows the system locale, not the UI language, so a
/// Japanese UI on an English system would otherwise get Chinese glyph forms.
/// Names that a system lacks are skipped by GPUI, so one list serves the
/// platform's own fonts and the common Noto/Source Han ones.
fn cjk_families(language: &str) -> &'static [&'static str] {
    match language {
        "zh-CN" => &[
            "Noto Sans CJK SC",
            "Source Han Sans SC",
            "Microsoft YaHei",
            "PingFang SC",
            "WenQuanYi Micro Hei",
        ],
        "ja" => &[
            "Noto Sans CJK JP",
            "Source Han Sans JP",
            "Yu Gothic",
            "Hiragino Sans",
            "Meiryo",
        ],
        "ko" => &[
            "Noto Sans CJK KR",
            "Source Han Sans KR",
            "Malgun Gothic",
            "Apple SD Gothic Neo",
        ],
        _ => &[],
    }
}

/// Puts the current language's CJK fonts behind the bundled ones for the
/// element and its children; call it where `font_family` sets `SANS`.
pub fn with_cjk_fallbacks<E>(mut element: E) -> E
where
    E: gpui::Styled,
{
    let families = cjk_families(&rust_i18n::locale());
    if !families.is_empty() {
        element.text_style().font_fallbacks = Some(FontFallbacks::from_fonts(
            families.iter().map(|family| (*family).to_owned()).collect(),
        ));
    }
    element
}

/// The Oswald 600 face every heading-like label uses, at `size` px.
pub fn display(size: f32) -> Div {
    div()
        .font_family(DISPLAY)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(size))
}

pub fn mono() -> Div {
    div().font_family(MONO)
}

pub fn muted(text: impl Into<SharedString>) -> Div {
    div().text_color(rgb(TEXT_MUTED)).child(text.into())
}

/// `.page`: a centred 960px column, padded 32/24/64, scrolling on its own.
pub fn page(id: &'static str, column: Div) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .child(column.w_full().max_w(px(960.)).mx_auto().pt_8().px_6().pb(px(64.)))
}

/// `.card`: the bordered panel lists sit in.
pub fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .bg(rgb(BG_PANEL_2))
        .border_1()
        .border_color(rgb(LINE))
        .rounded(px(RADIUS))
        .overflow_hidden()
}

/// `.list-row`: one row of a `.card`, divided from the next, lit on hover.
/// `last` drops the divider, standing in for `:last-child`, and rounds the
/// hover to the card's corners, since GPUI clips children to a square.
pub fn list_row(id: impl Into<gpui::ElementId>, last: bool) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_4()
        .px(px(18.))
        .py(px(14.))
        .text_color(rgb(TEXT_PRIMARY))
        .when(!last, |row| row.border_b_1().border_color(rgb(LINE)))
        .when(last, |row| row.rounded_b(px(RADIUS_SM)))
        .hover(|row| row.bg(rgb(BG_PANEL)))
}

/// A `.list-row` at settings density, without the hover: the rows of the PS
/// section's settings cards, which hold a dozen controls each.
pub fn compact_row(last: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(18.))
        .py(px(10.))
        .when(!last, |row| row.border_b_1().border_color(rgb(LINE)))
}

/// `.empty`: the dashed "nothing here" box.
pub fn empty(text: impl Into<SharedString>) -> Div {
    div()
        .text_color(rgb(TEXT_MUTED))
        .bg(rgb(BG_PANEL_2))
        .border_1()
        .border_dashed()
        .border_color(rgb(LINE))
        .rounded(px(RADIUS))
        .p(px(20.))
        .child(text.into())
}

/// `.error`: the red box.
pub fn error(text: impl Into<SharedString>) -> Div {
    div()
        .text_color(rgb(ERROR_TEXT))
        .bg(rgb(ERROR_BG))
        .border_1()
        .border_color(rgb(ERROR_LINE))
        .rounded(px(RADIUS))
        .px_4()
        .py_3()
        .child(text.into())
}

/// `.badge` / `.badge-gog`.
pub fn badge(text: impl AsRef<str>, gog: bool) -> Div {
    div()
        .flex_none()
        .text_size(px(10.))
        .font_weight(FontWeight::BOLD)
        .px(px(9.))
        .py(px(3.))
        .rounded(px(RADIUS_SM))
        .bg(rgb(BG_PANEL))
        .border_1()
        .border_color(rgb(if gog { ACCENT_BLUE_DIM } else { LINE }))
        .text_color(rgb(if gog { ACCENT_BLUE } else { TEXT_MUTED }))
        .child(text.as_ref().to_uppercase())
}
