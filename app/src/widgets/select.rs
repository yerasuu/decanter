//! `<select>`, drawn: a trigger styled like the webview's (our own chevron,
//! no native widget) that opens its options as a popup under itself.
//!
//! Stateless from the caller's side — the value comes in, changes go out
//! through `on_change` — with the open flag and the trigger's last bounds
//! kept as element state under the select's id.

use std::rc::Rc;

use gpui::{
    App, Bounds, Div, ElementId, FontWeight, IntoElement, Pixels, RenderOnce, SharedString, Stateful, Window, canvas,
    div, point, prelude::*, px, rgb,
};

use super::menu;
use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SelectStyle {
    /// Plain `select`: input-like field, muted chevron.
    Field,
    /// `#profile-switcher`: the topbar's, 12px, at most 200px wide.
    Compact,
    /// `.shelf-source`: a shelf title that happens to be operable —
    /// Oswald 15px uppercase, bright chevron, blue underline on hover.
    Heading,
    /// `.shelf-sort`: the small 11px one beside it.
    Small,
    /// `.kind-select`: the sidebar's product-type picker, 12px.
    Kind,
    /// `.fgroup select`: the filter panel's, 12.5px and full width.
    Group,
}

#[derive(Default)]
struct State {
    open: bool,
    bounds: Option<Bounds<Pixels>>,
}

type OnChange = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct Select {
    id: ElementId,
    options: Vec<(SharedString, SharedString)>,
    selected: SharedString,
    style: SelectStyle,
    full_width: bool,
    on_change: Option<OnChange>,
}

impl Select {
    /// `options` are `(value, label)`; `selected` is a value.
    pub fn new(
        id: impl Into<ElementId>,
        options: impl IntoIterator<Item = (SharedString, SharedString)>,
        selected: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            options: options.into_iter().collect(),
            selected: selected.into(),
            style: SelectStyle::Field,
            full_width: false,
            on_change: None,
        }
    }

    pub fn style(mut self, style: SelectStyle) -> Self {
        self.style = style;
        self
    }

    /// Stretches to its container instead of sizing to the longest option.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    fn options_list(self, min_width: Pixels, close: &menu::Dismiss) -> Stateful<Div> {
        div()
            .id((self.id, "options"))
            .flex()
            .flex_col()
            .min_w(min_width)
            .max_h(px(320.))
            .overflow_y_scroll()
            .p(px(4.))
            .bg(rgb(theme::BG_PANEL_2))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS))
            .shadow(theme::popup_shadow())
            .text_size(px(if matches!(self.style, SelectStyle::Field | SelectStyle::Heading) {
                13.
            } else {
                12.
            }))
            .children(self.options.into_iter().enumerate().map(|(index, (value, label))| {
                let current = value == self.selected;
                let on_change = self.on_change.clone();
                let close = Rc::clone(close);
                div()
                    .id(("option", index))
                    .px_2()
                    .py(px(5.))
                    .rounded(px(theme::RADIUS_SM))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .text_color(rgb(if current {
                        theme::TEXT_BRIGHT
                    } else {
                        theme::TEXT_PRIMARY
                    }))
                    .when(current, |option| option.bg(rgb(theme::BG_PANEL)))
                    .hover(|option| option.bg(rgb(theme::BG_PANEL_LIGHT)))
                    .on_click(move |_, window, cx| {
                        close(window, cx);
                        if !current && let Some(on_change) = &on_change {
                            on_change(&value, window, cx);
                        }
                    })
                    .child(label)
            }))
    }
}

impl RenderOnce for Select {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.id.clone(), cx, |_, _| State::default());
        let (open, bounds) = {
            let state = state.read(cx);
            (state.open, state.bounds)
        };
        let label = self
            .options
            .iter()
            .find(|(value, _)| *value == self.selected)
            .map(|(_, label)| label.clone())
            .unwrap_or_default();
        let style = self.style;
        let full_width = self.full_width;
        let heading = style == SelectStyle::Heading;
        let shown = |text: &SharedString| -> SharedString {
            if heading {
                text.to_uppercase().into()
            } else {
                text.clone()
            }
        };

        // The trigger is as wide as its longest option, the way a native
        // select sizes itself: every label is laid out at zero height
        // underneath the visible one, and the widest sets the width.
        let sizer = div().h_0().overflow_hidden().children(
            self.options
                .iter()
                .map(|(_, label)| div().whitespace_nowrap().child(shown(label))),
        );
        let chevron = div()
            .absolute()
            .top_0()
            .bottom_0()
            .flex()
            .items_center()
            .text_size(px(9.))
            .text_color(rgb(if heading { theme::TEXT_BRIGHT } else { theme::TEXT_MUTED }))
            .child("▾");

        let state_for_canvas = state.clone();
        let state_for_click = state.clone();
        let trigger = div()
            .id(self.id.clone())
            .relative()
            .flex()
            .flex_col()
            .cursor_pointer()
            .line_height(gpui::relative(1.4))
            .when(self.full_width, |trigger| trigger.w_full())
            .child(div().whitespace_nowrap().truncate().child(shown(&label)))
            .when(!self.full_width, |trigger| trigger.child(sizer))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        state_for_canvas.update(cx, |state, _| state.bounds = Some(bounds));
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .on_click(move |_, _, cx| {
                state_for_click.update(cx, |state, cx| {
                    state.open = !state.open;
                    cx.notify();
                });
            });
        let trigger = style_trigger(trigger, style, chevron);

        let popup = bounds.filter(|_| open).map(|bounds| {
            let close_state = state.clone();
            let close: menu::Dismiss = Rc::new(move |_, cx| {
                close_state.update(cx, |state, cx| {
                    state.open = false;
                    cx.notify();
                });
            });
            let list = self.options_list(bounds.size.width, &close);
            menu::popup(point(bounds.left(), bounds.bottom() + px(2.)), list, window, close)
        });

        div()
            .flex()
            .when(full_width, |select| select.w_full())
            .child(trigger)
            .children(popup)
    }
}

fn style_trigger(trigger: Stateful<Div>, style: SelectStyle, chevron: Div) -> Stateful<Div> {
    match style {
        SelectStyle::Field => trigger
            .pl(px(12.))
            .pr(px(26.))
            .py(px(9.))
            .bg(rgb(theme::BG_PANEL))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .child(chevron.right(px(9.))),
        SelectStyle::Compact => trigger
            .max_w(px(200.))
            .pl(px(10.))
            .pr(px(26.))
            .py(px(5.))
            .text_size(px(12.))
            .bg(rgb(theme::BG_PANEL))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .child(chevron.right(px(9.))),
        SelectStyle::Heading => trigger
            .max_w(px(320.))
            .pr(px(17.))
            .py(px(2.))
            .font_family(theme::DISPLAY)
            .font_weight(FontWeight::SEMIBOLD)
            .text_size(px(15.))
            .text_color(rgb(theme::TEXT_BRIGHT))
            .border_b_1()
            .border_color(gpui::transparent_black())
            .hover(|trigger| trigger.border_color(rgb(theme::ACCENT_BLUE)))
            .child(chevron.right(px(1.))),
        SelectStyle::Kind => trigger
            .pl(px(8.))
            .pr(px(24.))
            .py(px(6.))
            .text_size(px(12.))
            .bg(rgb(theme::BG_PANEL))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .child(chevron.right(px(9.))),
        SelectStyle::Group => trigger
            .pl(px(9.))
            .pr(px(26.))
            .py(px(7.))
            .text_size(px(12.5))
            .bg(rgb(theme::BG_PANEL))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .text_color(rgb(theme::TEXT_PRIMARY))
            .child(chevron.right(px(9.))),
        SelectStyle::Small => trigger
            .pl(px(6.))
            .pr(px(21.))
            .py(px(3.))
            .text_size(px(11.))
            .bg(rgb(theme::BG_PANEL_2))
            .border_1()
            .border_color(rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .text_color(rgb(theme::TEXT_MUTED))
            .hover(|trigger| {
                trigger
                    .text_color(rgb(theme::TEXT_PRIMARY))
                    .border_color(rgb(theme::ACCENT_BLUE_DIM))
            })
            .child(chevron.right(px(6.))),
    }
}
