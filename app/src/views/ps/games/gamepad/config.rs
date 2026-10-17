//! Configurations, as Steam Input lists them: the templates decanter ships
//! (read-only) and the user's own, kept in the layout store. Any of them can
//! be duplicated; the user's own can be renamed and deleted. The one picked
//! is the one in `bbport.ini`. While the drawing holds unapplied edits the
//! picker and these actions are locked, so a draft never lands on another
//! configuration than the one it was made from.

use decanter_ps::bbport::pad::PadLayout;
use gpui::{AnyElement, Context, Div, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::{CUSTOM, GamepadView, layout_name};
use crate::theme;
use crate::widgets::{ButtonKind, Select, button, select::SelectStyle};

/// A configuration's name in the picker, templates marked as such.
fn label(layout: &PadLayout) -> SharedString {
    let name = layout_name(&layout.id, layout.builtin, &layout.name);
    if layout.builtin {
        SharedString::from(t!("ps.gamepad.config.template", name = name))
    } else {
        SharedString::from(name.into_owned())
    }
}

impl GamepadView {
    pub(super) fn choose(&mut self, id: &str, cx: &mut Context<Self>) {
        let layout = self
            .loaded_mut()
            .and_then(|loaded| loaded.store.pad_layouts().into_iter().find(|layout| layout.id == id));
        if let Some(layout) = layout {
            self.apply(layout, false, cx);
        }
    }

    /// A new editable layout from the one in use, or from `bbport.ini`'s
    /// own lines when they match none.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let source = loaded.layout().unwrap_or_else(|| loaded.custom.clone());
        let name = t!(
            "ps.layouts.copy_name",
            name = layout_name(&source.id, source.builtin, &source.name)
        );
        let copy = loaded.store.copy_pad(&source, name.into_owned());
        self.apply(copy, true, cx);
    }

    pub(super) fn rename(&mut self, name: String, cx: &mut Context<Self>) {
        if self.draft.is_some() {
            return;
        }
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let Some(mut layout) = loaded.layout().filter(|layout| !layout.builtin && !name.is_empty()) else {
            return;
        };
        layout.name = name;
        loaded.store.update_pad(&layout);
        self.apply(layout, true, cx);
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let Some(layout) = loaded.layout().filter(|layout| !layout.builtin) else {
            return;
        };
        loaded.store.remove_pad(&layout.id);
        let [default, _] = PadLayout::builtins();
        self.apply(default, true, cx);
    }

    pub(super) fn layout_select(
        &self,
        layouts: &[PadLayout],
        current: Option<&PadLayout>,
        cx: &Context<Self>,
    ) -> AnyElement {
        if self.draft.is_some() {
            let name = current.map_or_else(|| SharedString::from(t!("ps.layouts.custom")), label);
            return theme::mono().child(name).into_any_element();
        }
        let entity = cx.entity();
        let selected = current.map_or_else(|| CUSTOM.to_owned(), |layout| layout.id.clone());
        let options = layouts
            .iter()
            .map(|layout| (SharedString::from(layout.id.clone()), label(layout)))
            .chain(
                current
                    .is_none()
                    .then(|| (SharedString::from(CUSTOM), SharedString::from(t!("ps.layouts.custom")))),
            );
        Select::new("ps-pad-layout", options, selected)
            .style(SelectStyle::Compact)
            .on_change(move |value, _, cx| {
                let id = value.to_string();
                entity.update(cx, |this, cx| this.choose(&id, cx));
            })
            .into_any_element()
    }

    pub(super) fn layout_actions(&self, own: bool, cx: &Context<Self>) -> Div {
        let locked = self.draft.is_some();
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .child(
                button(
                    "ps-pad-copy",
                    ButtonKind::Secondary,
                    t!("ps.gamepad.config.duplicate"),
                    locked,
                )
                .when(!locked, |copy| {
                    copy.on_click(cx.listener(|this, _, _, cx| this.copy(cx)))
                }),
            )
            .when(own, |actions| {
                actions
                    .child(div().w(px(220.)).child(self.name.clone()))
                    .child(
                        button(
                            "ps-pad-rename",
                            ButtonKind::Secondary,
                            t!("ps.gamepad.config.rename"),
                            locked,
                        )
                        .when(!locked, |rename| {
                            rename.on_click(cx.listener(|this, _, _, cx| {
                                let name = this.name.read(cx).text().trim().to_owned();
                                this.rename(name, cx);
                            }))
                        }),
                    )
                    .child(
                        button("ps-pad-remove", ButtonKind::Secondary, t!("ps.layouts.remove"), locked)
                            .when(!locked, |remove| {
                                remove.on_click(cx.listener(|this, _, _, cx| this.remove(cx)))
                            }),
                    )
            })
    }
}
