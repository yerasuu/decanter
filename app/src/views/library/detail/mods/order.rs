//! Each mod's slot in the account's load order and moving it: ↑ and ↓, or its handle dragged onto
//! another row. A move that makes another mod win a file or a DLL asks first, listing what changes
//! hands (mod packs design, section 5.4); one that changes no winner applies at once.

use decanter_mods::{ModId, OrderChange};
use gpui::{BoxShadow, Context, Div, SharedString, Window, div, point, prelude::*, px};
use rust_i18n::t;

use super::dll::mode_label;
use super::{ModsTab, Row};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, button, confirm_panel, tooltip};

/// How many files and DLLs changing hands the confirmation names before summing up the rest.
const SHOWN_CHANGES: usize = 12;

/// A move waiting for the user's answer, with what it changes.
pub(super) struct PendingMove {
    id: ModId,
    slot: usize,
    change: OrderChange,
}

/// A row's handle being dragged onto another row, which takes that row's slot.
#[derive(Clone)]
struct DraggedMod {
    id: ModId,
    name: SharedString,
}

struct DragGhost(SharedString);

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .bg(gpui::rgb(theme::BG_PANEL_2))
            .border_1()
            .border_color(gpui::rgb(theme::LINE))
            .rounded(px(theme::RADIUS_SM))
            .opacity(0.85)
            .text_size(px(12.5))
            .text_color(gpui::rgb(theme::TEXT_PRIMARY))
            .child(self.0.clone())
    }
}

impl ModsTab {
    /// The slot number, the drag handle and ↑ ↓, for a mod in this account's load order.
    pub(super) fn order_controls(&self, index: usize, row: &Row, locked: bool, cx: &Context<Self>) -> Option<Div> {
        let slot = row.position?;
        let count = self.loaded.as_ref().map_or(0, |loaded| loaded.slot_count);
        let first = slot == 0 || locked;
        let last = slot.saturating_add(1) >= count || locked;
        let (earlier, later) = (row.manifest.id.clone(), row.manifest.id.clone());
        let dragged = DraggedMod {
            id: row.manifest.id.clone(),
            name: row.manifest.name.clone().into(),
        };
        let handle = div()
            .id(("mod-drag", index))
            .px_1()
            .text_color(gpui::rgb(theme::TEXT_MUTED))
            .child("⠿")
            .when(!locked, |handle| {
                handle
                    .cursor_grab()
                    .tooltip(tooltip(t!("mods.order.drag")))
                    .on_drag(dragged, |dragged: &DraggedMod, _, _, cx| {
                        cx.new(|_| DragGhost(dragged.name.clone()))
                    })
            });
        Some(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    theme::muted(t!("mods.order.slot", slot = slot.saturating_add(1)))
                        .min_w(px(24.))
                        .text_size(px(12.5)),
                )
                .child(handle)
                .child(
                    button(("mod-earlier", index), ButtonKind::Icon, "↑", first)
                        .tooltip(tooltip(t!("mods.order.earlier")))
                        .when(!first, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.request_place(earlier.clone(), slot.saturating_sub(1), cx);
                            }))
                        }),
                )
                .child(
                    button(("mod-later", index), ButtonKind::Icon, "↓", last)
                        .tooltip(tooltip(t!("mods.order.later")))
                        .when(!last, |b| {
                            b.on_click(cx.listener(move |this, _, _, cx| {
                                this.request_place(later.clone(), slot.saturating_add(1), cx);
                            }))
                        }),
                ),
        )
    }

    /// A row in the load order takes a dragged mod into its slot, marked by a line on its edge
    /// toward where the mod comes from.
    pub(super) fn drop_target(item: Div, row: &Row, locked: bool, cx: &Context<Self>) -> Div {
        let Some(slot) = row.position.filter(|_| !locked) else {
            return item;
        };
        let target = row.manifest.id.clone();
        item.drag_over::<DraggedMod>(move |style, dragged, _, _| {
            if dragged.id == target {
                style
            } else {
                style.shadow(vec![BoxShadow {
                    color: gpui::rgb(theme::ACCENT_BLUE).into(),
                    offset: point(px(0.), px(-2.)),
                    blur_radius: px(0.),
                    spread_radius: px(0.),
                    inset: false,
                }])
            }
        })
        .on_drop(cx.listener(move |this, dragged: &DraggedMod, _, cx| {
            this.request_place(dragged.id.clone(), slot, cx);
        }))
    }

    /// Works out off the UI which files and DLLs the move would hand to another mod: none, and it
    /// applies; some, and the user is asked first.
    fn request_place(&mut self, id: ModId, slot: usize, cx: &mut Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        if self.is_locked() {
            return;
        }
        let account_id = self.target.account_id.clone();
        self.busy = Some(Text::new("mods.order.checking"));
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let preview_id = id.clone();
            let preview = tokio::task::spawn_blocking(move || mods.preview_place(&account_id, &preview_id, slot)).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match preview {
                    Ok(Ok(change)) if change.is_empty() => this.place(id, slot, cx),
                    Ok(Ok(change)) => {
                        tracing::info!(
                            mod_id = %id,
                            slot,
                            files = change.files.len(),
                            dlls = change.dlls.len(),
                            "load order move changes winners, asking first"
                        );
                        this.confirm_move = Some(PendingMove { id, slot, change });
                    }
                    Ok(Err(err)) => {
                        tracing::error!(error = %err, "could not check the load order move");
                        this.error = Some(Text::verbatim(err));
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "checking the load order move stopped");
                        this.error = Some(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn place(&mut self, id: ModId, slot: usize, cx: &mut Context<Self>) {
        self.confirm_move = None;
        self.run(
            Text::new("library.detail.mods.applying"),
            move |mods, account| mods.place(account, &id, slot),
            cx,
        );
    }

    pub(super) fn move_confirmation(&self, cx: &Context<Self>) -> Option<Div> {
        let pending = self.confirm_move.as_ref()?;
        let names = &self.loaded.as_ref()?.names;
        let name_of = |id: &ModId| names.get(id).cloned().unwrap_or_else(|| id.to_string());
        let change = &pending.change;
        let mut lines = Vec::with_capacity(SHOWN_CHANGES.saturating_add(3));
        if !change.files.is_empty() {
            lines.push(t!("mods.order.files_change", count = change.files.len()).into_owned());
        }
        lines.extend(change.files.iter().take(SHOWN_CHANGES).map(|moved| {
            t!(
                "mods.order.file_line",
                path = moved.path,
                before = name_of(&moved.before),
                after = name_of(&moved.after)
            )
            .into_owned()
        }));
        if let Some(rest) = change.files.len().checked_sub(SHOWN_CHANGES).filter(|&rest| rest > 0) {
            lines.push(t!("library.detail.mods.more", count = rest).into_owned());
        }
        if !change.dlls.is_empty() {
            lines.push(t!("mods.order.dlls_change", count = change.dlls.len()).into_owned());
        }
        lines.extend(change.dlls.iter().take(SHOWN_CHANGES).map(|moved| {
            t!(
                "mods.order.dll_line",
                dll = moved.after.dll,
                before = name_of(&moved.before.from),
                before_mode = mode_label(moved.before.mode),
                after = name_of(&moved.after.from),
                after_mode = mode_label(moved.after.mode)
            )
            .into_owned()
        }));
        if let Some(rest) = change.dlls.len().checked_sub(SHOWN_CHANGES).filter(|&rest| rest > 0) {
            lines.push(t!("library.detail.mods.more", count = rest).into_owned());
        }
        let (id, slot) = (pending.id.clone(), pending.slot);
        Some(confirm_panel(
            "mod-move-confirm",
            t!("mods.order.confirm_title", name = name_of(&pending.id)),
            lines.join("\n"),
            t!("mods.order.confirm"),
            cx.listener(move |this, _, _, cx| this.place(id.clone(), slot, cx)),
            cx.listener(|this, _, _, cx| {
                this.confirm_move = None;
                cx.notify();
            }),
        ))
    }

    /// The mods a hard constraint wants before this one that the load order puts after it.
    pub(super) fn constraint_block(row: &Row) -> Option<Div> {
        (!row.unmet.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pl(px(29.))
                .text_size(px(12.5))
                .children(
                    row.unmet
                        .iter()
                        .map(|name| theme::error(t!("mods.order.needs_first", name = name))),
                )
        })
    }
}
