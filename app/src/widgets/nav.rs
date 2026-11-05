//! Moving around without a pointer, with the keys a Steam Input keyboard layout sends for a pad
//! (the d-pad as arrows, A as Enter, B as Escape). GPUI already clicks the focused element on
//! Enter or Space; this moves the focus between tab stops and turns Escape into "back".
//!
//! Read from keystrokes nothing else handled, so a text field keeps its arrows, a popup its
//! Escape and the PS keyboard mapping every key it is capturing.

use gpui::{App, BoxShadow, InteractiveElement, Keystroke, Modifiers, Styled as _, Window, point, px, rgb};

use crate::theme;

#[derive(Clone, Copy)]
pub enum Step {
    Next,
    Prev,
}

impl Step {
    pub fn of(keystroke: &Keystroke) -> Option<Self> {
        let modifiers = keystroke.modifiers;
        if keystroke.key == "tab" && modifiers == Modifiers::shift() {
            return Some(Self::Prev);
        }
        if modifiers.modified() {
            return None;
        }
        match keystroke.key.as_str() {
            "tab" | "down" | "right" => Some(Self::Next),
            "up" | "left" => Some(Self::Prev),
            _ => None,
        }
    }

    pub fn apply(self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Next => window.focus_next(cx),
            Self::Prev => window.focus_prev(cx),
        }
    }
}

pub fn is_escape(keystroke: &Keystroke) -> bool {
    keystroke.key == "escape" && !keystroke.modifiers.modified()
}

/// The keyboard's context-menu key, or Shift+F10 where there is none: what a pad mapped to keys
/// presses in place of a right click.
pub fn is_menu_key(keystroke: &Keystroke) -> bool {
    match keystroke.key.as_str() {
        "menu" => !keystroke.modifiers.modified(),
        "f10" => keystroke.modifiers == Modifiers::shift(),
        _ => false,
    }
}

/// Makes `element` reachable with Tab and the arrows, ringed while it holds the keyboard's focus.
/// The ring is a shadow so it takes no room from the layout.
pub fn tab_stop<E: InteractiveElement>(element: E) -> E {
    focus_ring(element.tab_index(0))
}

/// The ring alone, for an element that tracks a focus handle of its own.
pub fn focus_ring<E: InteractiveElement>(element: E) -> E {
    element.focus_visible(|style| {
        style.shadow(vec![BoxShadow {
            color: rgb(theme::ACCENT_BLUE).into(),
            offset: point(px(0.), px(0.)),
            blur_radius: px(0.),
            spread_radius: px(2.),
            inset: false,
        }])
    })
}

/// Tab and the arrows move the focus and Escape calls `back`, in every window, for whatever
/// keystroke no element or action took first.
pub fn observe(cx: &mut App, back: impl Fn(&mut Window, &mut App) + 'static) {
    cx.observe_keystrokes(move |event, window, cx| {
        if event.action.is_some() {
            return;
        }
        if let Some(step) = Step::of(&event.keystroke) {
            step.apply(window, cx);
        } else if is_escape(&event.keystroke) {
            back(window, cx);
        }
    })
    .detach();
}
