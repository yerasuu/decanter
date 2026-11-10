//! The pieces every provider's panel is drawn with, so that a result, a detail, a filter, a failure
//! and the request budget look and behave the same whichever provider answered.

use gpui::{Context, Div, ObjectFit, SharedString, Stateful, div, img, prelude::*, px, rgb};
use rust_i18n::t;

use super::super::ModsTab;
use super::super::credits::{self, Platform};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, Select, button, checkbox};

pub(in crate::views::library::detail::mods) fn picture(url: Option<&str>, size: f32) -> Div {
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

pub(in crate::views::library::detail::mods) fn summary(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max_chars).collect();
    cut.push('…');
    cut
}

/// What *Try again* runs.
pub(in crate::views::library::detail::mods) type Retry = Box<dyn Fn(&mut ModsTab, &mut Context<ModsTab>)>;

/// One search result as every provider shows it.
pub(in crate::views::library::detail::mods) struct ResultItem<'item> {
    pub(in crate::views::library::detail::mods) picture: Option<&'item str>,
    pub(in crate::views::library::detail::mods) name: &'item str,
    pub(in crate::views::library::detail::mods) by: Option<String>,
    pub(in crate::views::library::detail::mods) summary: String,
    pub(in crate::views::library::detail::mods) stats: String,
    /// Warnings in the error colour: deprecated, NSFW, unavailable.
    pub(in crate::views::library::detail::mods) flags: Vec<String>,
    pub(in crate::views::library::detail::mods) platform: Platform,
    pub(in crate::views::library::detail::mods) is_adult: bool,
    /// The version of it the game already has.
    pub(in crate::views::library::detail::mods) installed: Option<String>,
}

pub(in crate::views::library::detail::mods) fn result_row(
    id: (&'static str, usize),
    item: ResultItem<'_>,
    on_click: impl Fn(&mut ModsTab, &mut Context<ModsTab>) + 'static,
    cx: &Context<ModsTab>,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .gap_3()
        .py(px(6.))
        .cursor_pointer()
        .border_t_1()
        .border_color(rgb(theme::LINE))
        .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
        .child(picture(item.picture, 40.))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(item.name.to_owned())
                        .children(item.by.map(theme::muted))
                        .child(credits::source(item.platform))
                        .children(item.is_adult.then(credits::adult_badge))
                        .children(
                            item.installed
                                .map(|version| theme::muted(t!("mods.search.installed", version = version))),
                        ),
                )
                .child(theme::muted(item.summary).text_size(px(12.5)))
                .child(theme::muted(item.stats).text_size(px(12.)))
                .children((!item.flags.is_empty()).then(|| theme::error(item.flags.join(" · ")))),
        )
}

/// The top of a detail: its picture beside the name and the lines under it.
pub(in crate::views::library::detail::mods) fn detail_head(
    url: Option<&str>,
    name: &str,
    lines: impl IntoIterator<Item = Div>,
) -> Div {
    div().flex().gap_3().child(picture(url, 64.)).child(
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(name.to_owned())
            .children(lines),
    )
}

/// The tab's sections: one button each, the shown one filled, a platform's name spelled as the
/// platform spells it.
pub(in crate::views::library::detail::mods) fn segments<T>(
    id: &'static str,
    items: &[(T, SharedString)],
    active: T,
    on_select: impl Fn(&mut ModsTab, T, &mut Context<ModsTab>) + Clone + 'static,
    cx: &Context<ModsTab>,
) -> Div
where
    T: Copy + PartialEq + 'static,
{
    div()
        .flex()
        .flex_wrap()
        .gap(px(6.))
        .children(items.iter().enumerate().map(|(index, &(item, ref label))| {
            let on_select = on_select.clone();
            let kind = if item == active {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            };
            button((id, index), kind, label.clone(), false)
                .on_click(cx.listener(move |this, _, _, cx| on_select(this, item, cx)))
        }))
}

/// A provider's whole section: its name and `top` (the mapped game, its buttons, the budget), then
/// `body`, then the line saying whose mods these are.
pub(in crate::views::library::detail::mods) fn section_frame(platform: Platform, top: Div, body: Div) -> Div {
    theme::card()
        .gap_3()
        .px(px(18.))
        .py(px(14.))
        .w_full()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_4()
                .child(div().font_weight(gpui::FontWeight::BOLD).child(platform.name()))
                .child(div().flex_1().min_w_0().child(top)),
        )
        .child(body)
        .child(credits::footer(platform))
}

/// Results on the left, the chosen mod on the right, one under the other when the window is narrow.
pub(in crate::views::library::detail::mods) fn split(list: Div, detail: Div) -> Div {
    let column = || div().flex().flex_col().gap_2().flex_1().flex_basis(px(380.)).min_w_0();
    div()
        .flex()
        .flex_wrap()
        .items_start()
        .gap_6()
        .child(column().child(list))
        .child(
            column()
                .p(px(12.))
                .rounded(px(theme::RADIUS))
                .bg(rgb(theme::BG_PANEL))
                .child(detail),
        )
}

pub(in crate::views::library::detail::mods) fn nothing_chosen() -> Div {
    theme::muted(t!("mods.search.nothing_chosen"))
}

/// The heading over a picker's entries that do not look like the game, shown only after the user
/// searched by hand.
pub(in crate::views::library::detail::mods) fn other_results() -> Div {
    theme::muted(t!("mods.search.other_results")).pt_2()
}

/// The row the filter and sort controls wrap in.
pub(in crate::views::library::detail::mods) fn controls() -> Div {
    div().flex().flex_wrap().items_center().gap(px(10.))
}

/// A labelled selector over `options`; picking one hands its value to `on_change`.
pub(in crate::views::library::detail::mods) fn choice<T>(
    id: &'static str,
    label: SharedString,
    options: Vec<(T, SharedString)>,
    selected: T,
    on_change: impl Fn(&mut ModsTab, T, &mut Context<ModsTab>) + 'static,
    cx: &Context<ModsTab>,
) -> Div
where
    T: Copy + PartialEq + 'static,
{
    let entity = cx.entity();
    let current = options
        .iter()
        .position(|(value, _)| *value == selected)
        .unwrap_or_default();
    let values: Vec<T> = options.iter().map(|&(value, _)| value).collect();
    let select = Select::new(
        id,
        options
            .into_iter()
            .enumerate()
            .map(|(index, (_, shown))| (SharedString::from(index.to_string()), shown)),
        current.to_string(),
    )
    .on_change(move |chosen, _, cx| {
        let Some(&value) = chosen.parse::<usize>().ok().and_then(|index| values.get(index)) else {
            return;
        };
        entity.update(cx, |this, cx| on_change(this, value, cx));
    });
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(theme::muted(label))
        .child(select)
}

/// A checkbox with its label, both clickable.
pub(in crate::views::library::detail::mods) fn flag(
    id: &'static str,
    label: SharedString,
    is_on: bool,
    on_toggle: impl Fn(&mut ModsTab, bool, &mut Context<ModsTab>) + 'static,
    cx: &Context<ModsTab>,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| on_toggle(this, !is_on, cx)))
        .child(checkbox(is_on))
        .child(theme::muted(label))
}

/// How many results show of how many matched, or that nothing did.
pub(in crate::views::library::detail::mods) fn count_line(shown: usize, total: Option<u64>) -> Div {
    let line = match total {
        Some(0) => t!("mods.search.none"),
        _ if shown == 0 => t!("mods.search.none"),
        Some(total) if total > shown as u64 => t!("mods.search.results_of", shown = shown, count = total),
        _ => t!("mods.search.results", count = shown),
    };
    theme::muted(line)
}

pub(in crate::views::library::detail::mods) fn load_more(
    id: &'static str,
    on_click: impl Fn(&mut ModsTab, &mut Context<ModsTab>) + 'static,
    cx: &Context<ModsTab>,
) -> Div {
    div().flex().pt_2().child(
        button(id, ButtonKind::Secondary, t!("mods.search.load_more"), false)
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx))),
    )
}

/// A failure's line, with *Try again* when asking again may help and whatever else goes with it.
pub(in crate::views::library::detail::mods) fn failure_line(
    id: &'static str,
    text: &Text,
    retry: Option<Retry>,
    extra: Option<Div>,
    cx: &Context<ModsTab>,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(theme::error(text))
        .when(retry.is_some() || extra.is_some(), |block| {
            block.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(10.))
                    .children(retry.map(|retry| {
                        button(id, ButtonKind::Secondary, t!("mods.search.retry"), false)
                            .on_click(cx.listener(move |this, _, _, cx| retry(this, cx)))
                    }))
                    .children(extra),
            )
        })
}

/// What a provider's allowance has left, as it last said.
pub(in crate::views::library::detail::mods) fn quota_line(text: impl Into<SharedString>) -> Div {
    theme::muted(text).text_size(px(12.))
}
