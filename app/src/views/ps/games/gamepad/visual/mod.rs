//! The bindings laid out like a console's control settings: a line-art
//! pad in the middle ([`drawing`]), the rows of each side around it and the
//! touchpad and Options under it ([`rows`]), tabs for the game's bindings
//! and the port's own, and the selected row's rebind under the grid. Edits
//! here are staged until Aplicar.

mod drawing;
mod rows;

use decanter_core::capabilities::Feature;
use decanter_ps::bbport::icons::IconStyle;
use decanter_ps::bbport::mods::icon_style;
use decanter_ps::bbport::pad::{INPUTS, MENU, PadLayout};
use decanter_ps::bbport::values::Values;
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, div, prelude::*, px, rgb};
use rust_i18n::t;

use super::staged::Scope;
use super::{GamepadView, gamepad_setting};
use crate::capabilities::Caps;
use crate::theme;
use crate::widgets::{ButtonKind, button};

/// What sits under the drawing on each tab.
const GAME_CENTRE: [&str; 2] = ["touchpad", "options"];
const PORT_CENTRE: [&str; 1] = [MENU.0];

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

    pub(super) fn visual(&self, values: &Values, cx: &Context<Self>) -> AnyElement {
        let (Some(layout), Some(saved)) = (self.edited(), self.saved()) else {
            return div().into_any_element();
        };
        let style = self.pad_style(values);
        let tabbed = Caps::get(cx).is_offered(Feature::PsMenuBinding);
        let scope = if tabbed { self.scope } else { Scope::Game };
        let centre: &[&'static str] = match scope {
            Scope::Game => &GAME_CENTRE,
            Scope::Port => &PORT_CENTRE,
        };
        let side = |side: &rows::Side| match scope {
            Scope::Game => self.side(side, &layout, &saved, style, cx),
            Scope::Port => div().flex_1(),
        };
        let grid = div()
            .flex()
            .items_start()
            .gap_4()
            .child(side(&rows::LEFT))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .w(px(drawing::WIDTH))
                    .gap_4()
                    .child(self.drawing(&layout, style, scope, cx))
                    .child(self.centre_rows(centre, &layout, &saved, style, cx)),
            )
            .child(side(&rows::RIGHT));
        theme::card()
            .p_5()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        theme::display(16.)
                            .flex_1()
                            .text_color(rgb(theme::TEXT_BRIGHT))
                            .child(t!("ps.gamepad.visual.title")),
                    )
                    .child(
                        button(
                            "ps-pad-restore",
                            ButtonKind::Secondary,
                            t!("ps.gamepad.visual.restore"),
                            false,
                        )
                        .text_size(px(12.))
                        .on_click(cx.listener(|this, _, _, cx| this.restore(cx))),
                    ),
            )
            .when(tabbed, |card| card.child(Self::scope_tabs(scope, cx)))
            .child(grid)
            .child(self.editor(&layout, scope, cx))
            .child(self.footer(&saved, cx))
            .into_any_element()
    }

    fn choose_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        self.scope = scope;
        // The port's tab has a single row, so it opens on it.
        self.selected = (scope == Scope::Port).then_some(MENU.0);
        cx.notify();
    }

    /// The tabs between the shoulder chips the console switches them with.
    fn scope_tabs(scope: Scope, cx: &Context<Self>) -> Div {
        let other = if scope == Scope::Game { Scope::Port } else { Scope::Game };
        let shoulder = |id: &'static str, text: &'static str| {
            div()
                .id(id)
                .flex_none()
                .px_2()
                .py(px(3.))
                .rounded(px(theme::RADIUS_SM))
                .bg(rgb(theme::BG_PANEL_3))
                .border_1()
                .border_color(rgb(theme::LINE))
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(theme::TEXT_MUTED))
                .cursor_pointer()
                .hover(|chip| chip.text_color(rgb(theme::TEXT_BRIGHT)))
                .on_click(cx.listener(move |this, _, _, cx| this.choose_scope(other, cx)))
                .child(text)
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(shoulder("ps-pad-scope-l1", "L1"))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .p(px(2.))
                    .gap(px(2.))
                    .bg(rgb(theme::BG_PANEL_3))
                    .border_1()
                    .border_color(rgb(theme::LINE))
                    .rounded(px(theme::RADIUS))
                    .children(Scope::ALL.into_iter().map(|tab| {
                        let active = tab == scope;
                        div()
                            .id(SharedString::from(format!("ps-pad-scope-{}", tab.label())))
                            .flex_1()
                            .py(px(5.))
                            .rounded(px(theme::RADIUS_SM))
                            .text_center()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .text_color(rgb(if active { theme::TEXT_BRIGHT } else { theme::TEXT_MUTED }))
                            .when(active, |item| item.bg(rgb(theme::BG_PANEL_LIGHT)))
                            .when(!active, |item| {
                                item.hover(|item| item.text_color(rgb(theme::TEXT_BRIGHT)))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| this.choose_scope(tab, cx)))
                            .child(t!(tab.label()))
                    })),
            )
            .child(shoulder("ps-pad-scope-r1", "R1"))
    }

    /// The selected row's rebind, the same control the list puts on its
    /// rows, staging what is picked.
    fn editor(&self, layout: &PadLayout, scope: Scope, cx: &Context<Self>) -> Div {
        let picked = self
            .selected
            .filter(|input| scope.owns(input))
            .and_then(|input| INPUTS.iter().chain([&MENU]).find(|(name, ..)| *name == input));
        div()
            .flex()
            .items_center()
            .gap_3()
            .min_h(px(52.))
            .px_4()
            .py_2()
            .rounded(px(theme::RADIUS))
            .bg(rgb(theme::BG_PANEL_3))
            .border_1()
            .border_color(rgb(theme::LINE))
            .child(picked.map_or_else(
                || {
                    theme::muted(t!("ps.gamepad.visual.pick"))
                        .text_size(px(12.))
                        .into_any_element()
                },
                |&(input, _, label, action)| {
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_3()
                        .child(
                            theme::display(15.)
                                .flex_none()
                                .text_color(rgb(theme::TEXT_BRIGHT))
                                .child(t!(label)),
                        )
                        .child(div().flex_1().min_w_0().truncate().child(t!(action)))
                        .child(
                            theme::muted(t!("ps.gamepad.visual.button"))
                                .flex_none()
                                .text_size(px(12.)),
                        )
                        .child(Self::binding_control(layout, input, true, Self::stage, cx))
                        .into_any_element()
                },
            ))
    }

    /// Aplicar and Cancelar, live only while there is something staged, and
    /// how applying lands on the configuration in use.
    fn footer(&self, saved: &PadLayout, cx: &Context<Self>) -> Div {
        let staged = self.draft.is_some();
        let (hint, colour) = if staged {
            (t!("ps.gamepad.visual.pending"), theme::ACCENT_GOLD)
        } else if saved.builtin {
            (t!("ps.gamepad.visual.hint_template"), theme::TEXT_MUTED)
        } else {
            (t!("ps.gamepad.visual.hint_own"), theme::TEXT_MUTED)
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        button(
                            "ps-pad-apply",
                            ButtonKind::Primary,
                            t!("ps.gamepad.visual.apply"),
                            !staged,
                        )
                        .min_w(px(200.))
                        .when(staged, |apply| {
                            apply.on_click(cx.listener(|this, _, _, cx| this.commit(cx)))
                        }),
                    )
                    .child(
                        button(
                            "ps-pad-cancel",
                            ButtonKind::Secondary,
                            t!("ps.gamepad.visual.cancel"),
                            !staged,
                        )
                        .min_w(px(200.))
                        .when(staged, |cancel| {
                            cancel.on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
                        }),
                    ),
            )
            .child(div().text_size(px(12.)).text_color(rgb(colour)).child(hint))
    }
}
