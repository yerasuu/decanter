use gpui::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::{checkbox_large, nav};
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
        let indent = px(23.);
        let row_gap = if self.help.is_some() { px(3.) } else { px(12.) };
        let row = div()
            .id(self.id)
            .flex()
            .gap(px(10.))
            .mb(row_gap)
            .max_w(px(520.))
            .items_center()
            .min_h(px(36.))
            .child(checkbox_large(self.checked))
            .child(div().min_w_0().child(self.title));
        let row = if self.is_offered {
            let row = nav::tab_stop(row).cursor_pointer();
            match self.on_click {
                Some(handler) => row.on_click(handler),
                None => row,
            }
        } else {
            row.opacity(0.4)
        };
        let row = if self.nested { row.ml(indent) } else { row };
        let help = self.help.map(|help| {
            let help = theme::muted(help).mb(px(12.));
            let help = if self.nested { help.ml(indent) } else { help };
            if self.is_offered { help } else { help.opacity(0.4) }
        });
        div().flex().flex_col().child(row).children(help)
    }
}
