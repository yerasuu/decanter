//! The pad in line art: a PS4 controller outlined in the theme's dim line,
//! each PS4 input a spot where it sits, labelled with the button that
//! reaches it in the pad's own icons. Spots select their row.

use std::borrow::Cow;

use decanter_ps::bbport::icons::IconStyle;
use decanter_ps::bbport::pad::{INPUTS, MENU, PadLayout};
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::super::GamepadView;
use super::super::staged::Scope;
use crate::theme;
use crate::widgets::tooltip;

pub(super) const WIDTH: f32 = 300.;
pub(super) const HEIGHT: f32 = 200.;

/// Brand colours of the face buttons, which no theme token stands for.
const PS_CROSS: u32 = 0x007f_a8e8;
const PS_CIRCLE: u32 = 0x00e8_737a;
const PS_SQUARE: u32 = 0x00d9_8ac8;
const PS_TRIANGLE: u32 = 0x005f_c9a8;
const XBOX_A: u32 = 0x006c_c04a;
const XBOX_B: u32 = 0x00e2_483d;
const XBOX_X: u32 = 0x003b_8fe0;
const XBOX_Y: u32 = 0x00f2_c12e;

/// A box on the drawing, top-left corner first.
#[derive(Clone, Copy)]
struct Shape {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    radius: f32,
}

const fn shape(left: f32, top: f32, width: f32, height: f32, radius: f32) -> Shape {
    Shape {
        left,
        top,
        width,
        height,
        radius,
    }
}

/// The body and both grips, outlined as one silhouette.
const SILHOUETTE: [Shape; 3] = [
    shape(16., 36., 268., 100., 48.),
    shape(20., 84., 92., 112., 46.),
    shape(188., 84., 92., 112., 46.),
];

/// Share and the PS button, drawn for the likeness; bbport binds neither.
const DECOR: [Shape; 2] = [shape(86., 46., 10., 20., 5.), shape(143., 104., 14., 14., 7.)];

const SHOULDERS: [(&str, Shape); 4] = [
    ("l2", shape(44., 4., 60., 14., 7.)),
    ("r2", shape(196., 4., 60., 14., 7.)),
    ("l1", shape(40., 22., 68., 12., 6.)),
    ("r1", shape(192., 22., 68., 12., 6.)),
];

const CENTRE: [(&str, Shape); 7] = [
    ("touchpad", shape(104., 42., 92., 52., 6.)),
    ("options", shape(204., 46., 10., 20., 5.)),
    ("triangle", shape(230., 50., 22., 22., 11.)),
    ("square", shape(206., 72., 22., 22., 11.)),
    ("circle", shape(254., 72., 22., 22., 11.)),
    ("cross", shape(230., 94., 22., 22., 11.)),
    ("r3", shape(168., 112., 40., 40., 20.)),
];

/// A `PlayStation` pad keeps the d-pad up top and both sticks low.
const PS_LEFT: [(&str, Shape); 5] = [
    ("up", shape(55., 54., 18., 20., 3.)),
    ("down", shape(55., 94., 18., 20., 3.)),
    ("left", shape(33., 75., 20., 18., 3.)),
    ("right", shape(75., 75., 20., 18., 3.)),
    ("l3", shape(92., 112., 40., 40., 20.)),
];

/// Xbox and Nintendo pads put the left stick up top and the d-pad low.
const OFFSET_LEFT: [(&str, Shape); 5] = [
    ("up", shape(103., 112., 18., 20., 3.)),
    ("down", shape(103., 152., 18., 20., 3.)),
    ("left", shape(81., 133., 20., 18., 3.)),
    ("right", shape(123., 133., 20., 18., 3.)),
    ("l3", shape(42., 62., 40., 40., 20.)),
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

/// `buttons` (alternatives split by `,`, a chord by `+`) as `style` prints
/// them, with the colour of the first face button among them.
pub(super) fn press(style: IconStyle, buttons: &str) -> (String, Option<u32>) {
    let mut colour = None;
    let text = buttons
        .split([',', '+'])
        .map(|button| {
            let (text, face) = glyph(style, button);
            colour = colour.or(face);
            text
        })
        .collect::<Vec<_>>()
        .join(" + ");
    (text, colour)
}

/// The text colour of a binding: gold once moved off bbport's default, so
/// a moved binding reads as moved wherever it shows.
pub(super) const fn press_colour(changed: bool, face: Option<u32>) -> u32 {
    match (changed, face) {
        (true, _) => theme::ACCENT_GOLD,
        (false, Some(colour)) => colour,
        (false, None) => theme::TEXT_PRIMARY,
    }
}

fn placed(shape: Shape) -> Div {
    div()
        .absolute()
        .left(px(shape.left))
        .top(px(shape.top))
        .w(px(shape.width))
        .h(px(shape.height))
        .rounded(px(shape.radius))
}

/// One shape, shrunk by the outline's width and filled with the card's
/// colour: laid over the outlines, it hides where the shapes overlap.
fn inset(shape: Shape) -> Div {
    placed(Shape {
        left: shape.left + 1.,
        top: shape.top + 1.,
        width: shape.width - 2.,
        height: shape.height - 2.,
        radius: shape.radius - 1.,
    })
    .bg(rgb(theme::BG_PANEL_2))
}

impl GamepadView {
    pub(super) fn drawing(&self, layout: &PadLayout, style: IconStyle, scope: Scope, cx: &Context<Self>) -> Div {
        let left = if style == IconStyle::Ps { &PS_LEFT } else { &OFFSET_LEFT };
        div()
            .relative()
            .flex_none()
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .children(
                SILHOUETTE
                    .iter()
                    .map(|&shape| placed(shape).border_1().border_color(rgb(theme::TEXT_DIM))),
            )
            .children(SILHOUETTE.iter().map(|&shape| inset(shape)))
            .children(
                DECOR
                    .iter()
                    .map(|&shape| placed(shape).border_1().border_color(rgb(theme::LINE))),
            )
            .children(
                SHOULDERS
                    .iter()
                    .chain(&CENTRE)
                    .chain(left)
                    .map(|&spot| self.spot(spot, layout, style, scope, cx)),
            )
    }

    /// An input on the drawing. On the port's tab the inputs the menu's
    /// buttons also reach are lit, and any spot picks the menu's row.
    fn spot(
        &self,
        (input, shape): (&'static str, Shape),
        layout: &PadLayout,
        style: IconStyle,
        scope: Scope,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(&(_, default, label, action)) = INPUTS.iter().find(|(name, ..)| *name == input) else {
            return div().into_any_element();
        };
        let buttons = layout.buttons(input);
        let (text, face) = press(style, buttons);
        let pick = if scope == Scope::Port { MENU.0 } else { input };
        let selected = scope == Scope::Game && self.selected == Some(input);
        let lit = scope == Scope::Port
            && layout
                .buttons(MENU.0)
                .split(['+', ','])
                .any(|button| buttons.split(',').any(|held| held == button));
        let room = match shape.width {
            width if width >= 60. => 12,
            width if width >= 40. => 3,
            width if width >= 18. => 1,
            _ => 0,
        };
        let fits = text.chars().count() <= room;
        let border = if selected || lit {
            theme::ACCENT_BLUE
        } else {
            theme::TEXT_DIM
        };
        placed(shape)
            .id(SharedString::from(format!("ps-pad-spot-{input}")))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(border))
            .when(selected || lit, |spot| spot.bg(theme::alpha(theme::ACCENT_BLUE, 0.2)))
            .hover(|spot| spot.bg(theme::alpha(theme::ACCENT_BLUE, 0.12)))
            .cursor_pointer()
            .text_size(px(if shape.height >= 20. { 12. } else { 9. }))
            .font_weight(FontWeight::BOLD)
            .line_height(px(shape.height - 2.))
            .text_color(rgb(press_colour(buttons != default, face)))
            .tooltip(tooltip(format!("{} · {}", t!(label), t!(action))))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(pick);
                cx.notify();
            }))
            .when(fits, |spot| spot.child(text))
            .into_any_element()
    }
}
