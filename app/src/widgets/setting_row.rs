use gpui::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};

use super::{nav, switch};
use crate::theme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A settings checkbox with its title and optional help line, the way every Settings view draws one.
/// The row is a single tab stop that `on_click` toggles from the pointer, Enter or Space; the row is
/// the hit area, not just the box.
#[derive(IntoElement)]
pub struct SettingRow {
    id: ElementId,
    checked: bool,
    title: SharedString,
    help: Option<SharedString>,
    nested: bool,
    is_offered: bool,
    on_click: Option<ClickHandler>,
}

impl SettingRow {
    pub fn new(id: impl Into<ElementId>, checked: bool, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            checked,
            title: title.into(),
            help: None,
            nested: false,
            is_offered: true,
            on_click: None,
        }
    }

    pub fn help(mut self, help: impl Into<SharedString>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Indents the row under the switch above it. While `is_offered` is false (that switch is off) it
    /// shows its value dimmed and stops being a tab stop, so Tab and the d-pad skip it.
    pub fn nested(mut self, is_offered: bool) -> Self {
        self.nested = true;
        self.is_offered = is_offered;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for SettingRow {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let row = div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .max_w(px(720.))
            .min_h(px(52.))
            .px_4()
            .py(px(10.))
            .rounded(px(theme::RADIUS))
            .border_1()
            .border_color(rgb(theme::LINE))
            .bg(rgb(theme::BG_PANEL_2))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(div().text_color(rgb(theme::TEXT_PRIMARY)).child(self.title))
                    .children(self.help.map(|help| theme::muted(help).text_size(px(12.)))),
            )
            .child(switch(self.checked));
        let row = if self.is_offered {
            let row = nav::tab_stop(row)
                .cursor_pointer()
                .hover(|row| row.border_color(rgb(theme::ACCENT_BLUE_DIM)));
            match self.on_click {
                Some(handler) => row.on_click(handler),
                None => row,
            }
        } else {
            row.opacity(0.4)
        };
        let row = if self.nested {
            div()
                .ml(px(24.))
                .pl(px(12.))
                .border_l_2()
                .border_color(rgb(theme::ACCENT_BLUE_DIM))
                .child(row)
        } else {
            div().child(row)
        };
        row.mb(px(10.))
    }
}
