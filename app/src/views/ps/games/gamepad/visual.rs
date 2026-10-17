//! The bindings on a drawing of a pad: each PS4 input sits where it is on
//! the pad and shows the button that reaches it, labelled the way the pad
//! in hand (or the fixed icon style) labels its buttons. Under the default
//! layout the drawing reads like that pad; a moved binding shows where it
//! went, in gold.

use std::borrow::Cow;

use decanter_ps::bbport::icons::IconStyle;
use decanter_ps::bbport::mods::icon_style;
use decanter_ps::bbport::pad::{INPUTS, PadLayout};
use decanter_ps::bbport::values::Values;
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::{GamepadView, gamepad_setting};
use crate::theme;
use crate::widgets::tooltip;

const WIDTH: f32 = 520.;
const HEIGHT: f32 = 284.;

/// Brand colours of the face buttons, which no theme token stands for.
const PS_CROSS: u32 = 0x007f_a8e8;
const PS_CIRCLE: u32 = 0x00e8_737a;
const PS_SQUARE: u32 = 0x00d9_8ac8;
const PS_TRIANGLE: u32 = 0x005f_c9a8;
const XBOX_A: u32 = 0x006c_c04a;
const XBOX_B: u32 = 0x00e2_483d;
const XBOX_X: u32 = 0x003b_8fe0;
const XBOX_Y: u32 = 0x00f2_c12e;

/// One drawn input: its box on the drawing, top-left corner first.
struct Spot {
    input: &'static str,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    radius: f32,
}

const fn spot(input: &'static str, left: f32, top: f32, width: f32, height: f32, radius: f32) -> Spot {
    Spot {
        input,
        left,
        top,
        width,
        height,
        radius,
    }
}

const SHOULDERS: [Spot; 4] = [
    spot("l2", 72., 0., 104., 24., 10.),
    spot("r2", 344., 0., 104., 24., 10.),
    spot("l1", 72., 28., 104., 18., 6.),
    spot("r1", 344., 28., 104., 18., 6.),
];

const CENTRE: [Spot; 7] = [
    spot("touchpad", 198., 56., 124., 66., 8.),
    spot("options", 330., 58., 44., 18., 9.),
    spot("cross", 387., 123., 34., 34., 17.),
    spot("circle", 419., 91., 34., 34., 17.),
    spot("square", 355., 91., 34., 34., 17.),
    spot("triangle", 387., 59., 34., 34., 17.),
    spot("r3", 298., 152., 52., 52., 26.),
];

/// A `PlayStation` pad keeps the d-pad up top and both sticks low.
const PS_LEFT: [Spot; 5] = [
    spot("up", 103., 67., 26., 26., 4.),
    spot("down", 103., 123., 26., 26., 4.),
    spot("left", 75., 95., 26., 26., 4.),
    spot("right", 131., 95., 26., 26., 4.),
    spot("l3", 170., 152., 52., 52., 26.),
];

/// Xbox and Nintendo pads put the left stick up top and the d-pad low.
const OFFSET_LEFT: [Spot; 5] = [
    spot("up", 173., 135., 26., 26., 4.),
    spot("down", 173., 191., 26., 26., 4.),
    spot("left", 145., 163., 26., 26., 4.),
    spot("right", 201., 163., 26., 26., 4.),
    spot("l3", 90., 82., 52., 52., 26.),
];

/// How `style` prints SDL's `button`, and the colour of a face button.
fn glyph(style: IconStyle, button: &str) -> (Cow<'static, str>, Option<u32>) {
    let face = match (style, button) {
        (IconStyle::Ps, "a") => Some(("✕", Some(PS_CROSS))),
        (IconStyle::Ps, "b") => Some(("○", Some(PS_CIRCLE))),
        (IconStyle::Ps, "x") => Some(("□", Some(PS_SQUARE))),
        (IconStyle::Ps, "y") => Some(("△", Some(PS_TRIANGLE))),
        (IconStyle::Abxy, "a") => Some(("A", Some(XBOX_A))),
        (IconStyle::Abxy, "b") => Some(("B", Some(XBOX_B))),
        (IconStyle::Abxy, "x") => Some(("X", Some(XBOX_X))),
        (IconStyle::Abxy, "y") => Some(("Y", Some(XBOX_Y))),
        // SDL names the buttons by position, and Nintendo prints them
        // swapped: B on the south one, A on the east.
        (IconStyle::Bayx, "a") => Some(("B", None)),
        (IconStyle::Bayx, "b") => Some(("A", None)),
        (IconStyle::Bayx, "x") => Some(("Y", None)),
        (IconStyle::Bayx, "y") => Some(("X", None)),
        _ => None,
    };
    if let Some((text, colour)) = face {
        return (Cow::Borrowed(text), colour);
    }
    let [ps, abxy, bayx] = match button {
        "leftshoulder" => ["L1", "LB", "L"],
        "rightshoulder" => ["R1", "RB", "R"],
        "lefttrigger" => ["L2", "LT", "ZL"],
        "righttrigger" => ["R2", "RT", "ZR"],
        "leftstick" => ["L3", "LS", "LS"],
        "rightstick" => ["R3", "RS", "RS"],
        "back" => ["Share", "View", "−"],
        "start" => ["Options", "Menu", "+"],
        "misc1" => ["Mic", "Share", "Capture"],
        "dpup" => ["↑"; 3],
        "dpdown" => ["↓"; 3],
        "dpleft" => ["←"; 3],
        "dpright" => ["→"; 3],
        "paddle1" => ["P1"; 3],
        "paddle2" => ["P2"; 3],
        "paddle3" => ["P3"; 3],
        "touchpad" => return (t!("ps.gamepad.buttons.touchpad"), None),
        other => return (Cow::Owned(other.to_owned()), None),
    };
    let text = match style {
        IconStyle::Ps => ps,
        IconStyle::Abxy => abxy,
        IconStyle::Bayx => bayx,
    };
    (Cow::Borrowed(text), None)
}

/// The silhouette: a body and two grips in one fill, so they read as one shape.
fn shell(left: f32, top: f32, width: f32, height: f32, radius: f32) -> Div {
    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .h(px(height))
        .rounded(px(radius))
        .bg(rgb(theme::BG_PANEL_LIGHT))
}

impl GamepadView {
    /// The style the drawing labels buttons in: the fixed icon style when
    /// one is set, else the pad bbport will read (the one picked, else the
    /// first connected), as the launch picks it for the game's icons.
    fn pad_style(&self, values: &Values) -> IconStyle {
        let chosen = gamepad_setting().and_then(|setting| values.get(setting));
        let detected = chosen
            .as_deref()
            .and_then(|guid| {
                self.gamepads
                    .iter()
                    .find(|(known, _)| known == guid)
                    .map(|(_, name)| IconStyle::for_gamepad(guid, name))
                    .or_else(|| {
                        values
                            .launch()
                            .text("gamepad_name")
                            .map(|name| IconStyle::for_gamepad(guid, name))
                    })
            })
            .or_else(|| {
                self.gamepads
                    .first()
                    .map(|(guid, name)| IconStyle::for_gamepad(guid, name))
            });
        icon_style(values.launch(), detected)
            .or(detected)
            .unwrap_or(IconStyle::Abxy)
    }

    pub(super) fn visual(&self, layout: &PadLayout, values: &Values, cx: &Context<Self>) -> AnyElement {
        let style = self.pad_style(values);
        let left = if style == IconStyle::Ps { &PS_LEFT } else { &OFFSET_LEFT };
        let drawing = div()
            .relative()
            .flex_none()
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .child(shell(56., 110., 140., 170., 70.))
            .child(shell(324., 110., 140., 170., 70.))
            .child(shell(40., 46., 440., 160., 72.))
            .children(
                SHOULDERS
                    .iter()
                    .chain(&CENTRE)
                    .chain(left)
                    .map(|spot| self.spot(spot, layout, style, cx)),
            );
        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap_4()
            .child(theme::card().p_4().child(drawing))
            .child(self.panel(layout, cx))
            .into_any_element()
    }

    fn spot(&self, spot: &Spot, layout: &PadLayout, style: IconStyle, cx: &Context<Self>) -> AnyElement {
        let Some(&(input, default, label, action)) = INPUTS.iter().find(|(name, ..)| *name == spot.input) else {
            return div().into_any_element();
        };
        let buttons = layout.buttons(input);
        let changed = buttons != default;
        let selected = self.selected == Some(input);
        let mut colour = None;
        let text = buttons
            .split(',')
            .map(|button| {
                let (text, face) = glyph(style, button);
                colour = colour.or(face);
                text
            })
            .collect::<Vec<_>>()
            .join(" + ");
        let large = text.chars().count() == 1 && spot.height >= 24.;
        let text_colour = match (changed, colour) {
            (false, Some(colour)) => colour,
            (false, None) => theme::TEXT_PRIMARY,
            (true, _) => theme::ACCENT_GOLD,
        };
        let border = if selected {
            theme::ACCENT_BLUE
        } else if changed {
            theme::ACCENT_GOLD
        } else {
            theme::LINE
        };
        let hint = format!("{} · {}", t!(label), t!(action));
        div()
            .id(SharedString::from(format!("ps-pad-spot-{input}")))
            .absolute()
            .left(px(spot.left))
            .top(px(spot.top))
            .w(px(spot.width))
            .h(px(spot.height))
            .rounded(px(spot.radius))
            .flex()
            .items_center()
            .justify_center()
            .text_center()
            .overflow_hidden()
            .bg(rgb(if selected { theme::BG_PANEL_2 } else { theme::BG_PANEL }))
            .border_color(rgb(border))
            .when(selected, Styled::border_2)
            .when(!selected, Styled::border_1)
            .hover(|spot| spot.bg(rgb(theme::BG_PANEL_2)))
            .cursor_pointer()
            .text_size(px(if large { 16. } else { 10. }))
            .font_weight(if large { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
            .line_height(px(if large { 18. } else { 12. }))
            .text_color(rgb(text_colour))
            .tooltip(tooltip(hint))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(input);
                cx.notify();
            }))
            .child(text)
            .into_any_element()
    }

    /// The picked input's action and the control that rebinds it, the
    /// same one the list puts on its row.
    fn panel(&self, layout: &PadLayout, cx: &Context<Self>) -> Div {
        let body = self
            .selected
            .and_then(|input| INPUTS.iter().find(|(name, ..)| *name == input))
            .map_or_else(
                || theme::muted(t!("ps.gamepad.visual.pick")).into_any_element(),
                |&(input, _, label, action)| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_baseline()
                                .gap_3()
                                .child(theme::display(18.).text_color(rgb(theme::TEXT_BRIGHT)).child(t!(label)))
                                .child(div().min_w_0().child(t!(action))),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(theme::muted(t!("ps.gamepad.visual.button")).flex_1())
                                .child(Self::binding_control(layout, input, cx)),
                        )
                        .into_any_element()
                },
            );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(240.))
            .gap_3()
            .child(theme::card().p_4().child(body))
            .child(theme::muted(t!("ps.gamepad.visual.legend")).text_size(px(12.)))
    }
}
