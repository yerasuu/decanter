//! Configurations, as Steam Input lists them: the templates decanter ships (read-only) and the
//! user's own, kept in the shared layouts catalog. Any of them can be duplicated; the user's own can
//! be renamed, saved into and deleted, and deleting one a game uses asks first, naming the games.
//! Picking one copies its lines into the game's port ini and records it as the one the game follows.
//! While the drawing holds unapplied edits the picker and these actions are locked, so a draft never
//! lands on another configuration than the one it was made from.

use decanter_ps::bbport::pad::PadLayout;
use decanter_ps::layouts::{self, Kind, choice};
use gpui::{AnyElement, Context, Div, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::{CUSTOM, GamepadView, game_names, layout_name, same_lines};
use crate::theme;
use crate::widgets::{ButtonKind, Select, button, confirm_panel, select::SelectStyle};

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
            self.write_game(layout, None, cx);
        }
    }

    /// A new editable layout from the game's lines: the layout in use with the game's own changes,
    /// or the lines alone when they follow none.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.saved() else {
            return;
        };
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        let name = t!(
            "ps.layouts.copy_name",
            name = layout_name(&source.id, source.builtin, &source.name)
        );
        let copy = loaded.store.copy_pad(&source, name.into_owned());
        self.write_game(copy.clone(), Some(copy), cx);
    }

    /// The game's own changes into the user's layout it follows; games that took that layout
    /// earlier keep their copy (D4).
    fn save_to_layout(&mut self, cx: &mut Context<Self>) {
        if self.draft.is_some() {
            return;
        }
        let Some(lines) = self.saved().filter(|layout| !layout.builtin) else {
            return;
        };
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        loaded.store.update_pad(&lines);
        self.write_game(lines.clone(), Some(lines), cx);
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
        self.write_layout(layout, cx);
    }

    /// Asks first when a game uses the layout, naming them (#1133); deletes at once otherwise.
    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(layout) = self
            .loaded()
            .and_then(|loaded| loaded.layout())
            .filter(|layout| !layout.builtin)
        else {
            return;
        };
        let task = cx.background_spawn({
            let layout = layout.clone();
            async move { choice::pad_users(&decanter_ps::paths::active(), &layout) }
        });
        cx.spawn(async move |this, cx| {
            let users = task.await;
            let _ = this.update(cx, |this, cx| {
                if users.is_empty() {
                    this.delete(layout, cx);
                } else {
                    this.removing = Some((layout, users));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The game keeps its `pad.` lines as they are; it only stops following the layout.
    fn delete(&mut self, layout: PadLayout, cx: &mut Context<Self>) {
        self.removing = None;
        let Some(loaded) = self.loaded_mut() else {
            return;
        };
        loaded.store.remove_pad(&layout.id);
        loaded.current = loaded
            .store
            .pad_layouts()
            .into_iter()
            .find(|known| same_lines(known, &loaded.game))
            .map(|known| known.id);
        let task = cx.background_spawn(async move {
            layouts::remove(&decanter_ps::paths::active(), Kind::Pad, &layout.id).map(drop)
        });
        Self::report(task, cx);
        self.sync_name(cx);
        cx.notify();
    }

    pub(super) fn confirmation(&self, cx: &Context<Self>) -> Option<Div> {
        let (layout, users) = self.removing.as_ref()?;
        let entity = cx.entity();
        let cancel = cx.entity();
        Some(confirm_panel(
            "ps-pad-remove-confirm",
            t!("ps.layouts.confirm_remove.title", name = layout.name),
            t!(
                "ps.layouts.confirm_remove.body",
                games = game_names(self.ps.read(cx), users)
            ),
            t!("ps.layouts.confirm_remove.yes"),
            move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    if let Some((layout, _)) = this.removing.take() {
                        this.delete(layout, cx);
                    }
                });
            },
            move |_, _, cx| {
                cancel.update(cx, |this, cx| {
                    this.removing = None;
                    cx.notify();
                });
            },
        ))
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

    pub(super) fn layout_actions(&self, own: bool, has_own_changes: bool, cx: &Context<Self>) -> Div {
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
            .when(has_own_changes, |actions| {
                actions.child(
                    button(
                        "ps-pad-save-layout",
                        ButtonKind::Secondary,
                        t!("ps.layouts.save_to_layout"),
                        locked,
                    )
                    .when(!locked, |save| {
                        save.on_click(cx.listener(|this, _, _, cx| this.save_to_layout(cx)))
                    }),
                )
            })
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
