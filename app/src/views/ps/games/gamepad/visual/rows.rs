//! The rows around the drawing, as a console's control settings lay them
//! out: shoulders, d-pad or face buttons and the stick on each side, the
//! touchpad and Options (or the port's menu) under the pad. Each row is the
//! button that reaches the input and what the game does with it.

use decanter_ps::bbport::icons::IconStyle;
use decanter_ps::bbport::pad::{INPUTS, MENU, PadLayout};
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::super::GamepadView;
use super::drawing::{HEIGHT, press, press_colour};
use crate::theme;
use crate::widgets::tooltip;

const ROW_HEIGHT: f32 = 26.;

/// One side column: the shoulder pair, the cluster of four, and the stick
/// at the bottom, level with the items under the drawing.
pub(super) struct Side {
    shoulders: [&'static str; 2],
    cluster: [&'static str; 4],
    stick_press: &'static str,
    stick_letter: &'static str,
    stick_action: &'static str,
}

pub(super) const LEFT: Side = Side {
    shoulders: ["l1", "l2"],
    cluster: ["up", "left", "right", "down"],
    stick_press: "l3",
    stick_letter: "L",
    stick_action: "ps.gamepad.visual.left_stick",
};

pub(super) const RIGHT: Side = Side {
    shoulders: ["r1", "r2"],
    cluster: ["triangle", "square", "circle", "cross"],
    stick_press: "r3",
    stick_letter: "R",
    stick_action: "ps.gamepad.visual.right_stick",
};

/// The small rounded box a row opens with.
fn chip() -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_1()
        .min_w(px(44.))
        .h_full()
        .px_2()
        .rounded(px(theme::RADIUS_SM))
        .bg(rgb(theme::BG_PANEL_3))
        .border_1()
        .border_color(rgb(theme::LINE))
        .text_size(px(12.))
        .font_weight(FontWeight::BOLD)
        .whitespace_nowrap()
}

fn group(rows: impl IntoIterator<Item = AnyElement>) -> Div {
    div().flex().flex_col().gap(px(6.)).children(rows)
}

impl GamepadView {
    /// A side column, its top as tall as the drawing so the stick's group
    /// lines up with what sits under it.
    pub(super) fn side(
        &self,
        side: &Side,
        layout: &PadLayout,
        saved: &PadLayout,
        style: IconStyle,
        cx: &Context<Self>,
    ) -> Div {
        let rows =
            |inputs: &[&'static str]| group(inputs.iter().map(|&input| self.row(input, layout, saved, style, cx)));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .h(px(HEIGHT))
                    .child(rows(&side.shoulders))
                    .child(rows(&side.cluster)),
            )
            .child(group([
                self.row(side.stick_press, layout, saved, style, cx),
                stick_row(side.stick_letter, side.stick_action),
            ]))
    }

    /// The rows under the drawing.
    pub(super) fn centre_rows(
        &self,
        inputs: &[&'static str],
        layout: &PadLayout,
        saved: &PadLayout,
        style: IconStyle,
        cx: &Context<Self>,
    ) -> Div {
        group(inputs.iter().map(|&input| self.row(input, layout, saved, style, cx)))
    }

    /// `input`'s row: the button (or buttons) that reach it, gold when
    /// moved off the default, and the game's action, marked while the
    /// change is staged. Clicking it opens its rebind below the grid.
    fn row(
        &self,
        input: &'static str,
        layout: &PadLayout,
        saved: &PadLayout,
        style: IconStyle,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(&(_, default, label, action)) = INPUTS.iter().chain([&MENU]).find(|(name, ..)| *name == input) else {
            return div().into_any_element();
        };
        let buttons = layout.buttons(input);
        let (text, face) = press(style, buttons);
        let staged = buttons != saved.buttons(input);
        let selected = self.selected == Some(input);
        div()
            .id(SharedString::from(format!("ps-pad-row-{input}")))
            .flex()
            .items_center()
            .gap_2()
            .h(px(ROW_HEIGHT))
            .cursor_pointer()
            .tooltip(tooltip(format!("{} · {}", t!(label), t!(action))))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(input);
                cx.notify();
            }))
            .child(
                chip()
                    .text_color(rgb(press_colour(buttons != default, face)))
                    .child(text),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .rounded(px(theme::RADIUS_SM))
                    .border_1()
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .when(selected, |area| {
                        area.bg(rgb(theme::TEXT_PRIMARY))
                            .border_color(rgb(theme::TEXT_BRIGHT))
                            .text_color(rgb(theme::BG_DEEP))
                    })
                    .when(!selected, |area| {
                        area.bg(rgb(theme::BG_PANEL))
                            .border_color(rgb(theme::LINE))
                            .text_color(rgb(theme::TEXT_PRIMARY))
                            .hover(|area| area.bg(rgb(theme::BG_PANEL_LIGHT)))
                    })
                    .child(div().flex_1().min_w_0().truncate().child(t!(action)))
                    .when(staged, |area| {
                        area.child(
                            div()
                                .flex_none()
                                .size(px(6.))
                                .rounded_full()
                                .bg(rgb(theme::ACCENT_BLUE)),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// A stick's movement: shown for the whole picture, but bbport moves the
/// character and the camera with the sticks under any layout.
fn stick_row(letter: &'static str, action: &'static str) -> AnyElement {
    div()
        .id(SharedString::from(format!("ps-pad-stick-{letter}")))
        .flex()
        .items_center()
        .gap_2()
        .h(px(ROW_HEIGHT))
        .tooltip(tooltip(t!("ps.gamepad.visual.stick_fixed")))
        .child(
            chip()
                .text_color(rgb(theme::TEXT_MUTED))
                .child(
                    div()
                        .size(px(10.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(theme::TEXT_MUTED)),
                )
                .child(letter),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .items_center()
                .px_3()
                .rounded(px(theme::RADIUS_SM))
                .border_1()
                .border_color(rgb(theme::LINE))
                .text_size(px(12.))
                .text_color(rgb(theme::TEXT_MUTED))
                .child(div().min_w_0().truncate().child(t!(action))),
        )
        .into_any_element()
}
