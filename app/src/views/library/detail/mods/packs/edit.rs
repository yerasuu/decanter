//! Making and editing a pack: a new one from the installed mods, its name, and per entry its slot,
//! whether it is on, pinned or latest, or out of the pack. Each edit bumps the revision and is
//! written through the format's checks, so nothing is saved that the next read would refuse. A move
//! that breaks a hard edge is refused with the reason; one that hands a file or a DLL to another mod
//! asks first, as a move in the mod list does (design, section 5.4).

use decanter_modpack::{EntryKey, Pack, Track};
use decanter_mods::{ModId, OrderChange};
use gpui::{Context, Div};
use rust_i18n::t;

use super::{Naming, Open, open_data, run};
use crate::i18n::Text;
use crate::views::library::detail::mods::ModsTab;
use crate::views::library::detail::mods::dll::mode_label;
use crate::widgets::confirm_panel;

/// How many files and DLLs changing hands a confirmation names before summing up the rest.
const SHOWN_CHANGES: usize = 12;

/// A reorder waiting for the user's answer: the pack as it would be saved, and what it changes.
pub(super) struct PendingMove {
    pack: Pack,
    change: OrderChange,
}

impl ModsTab {
    pub(super) fn start_new_pack(&mut self, cx: &mut Context<Self>) {
        let title = self.target.title.clone();
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        let name = t!("mods.packs.default_name", game = title).into_owned();
        packs.name_input.update(cx, |input, cx| input.set_text(name, cx));
        packs.naming = Some(Naming::New);
        cx.notify();
    }

    pub(super) fn start_rename(&mut self, cx: &mut Context<Self>) {
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        let Some(name) = packs.open.as_ref().map(|open| open.pack.name.clone()) else {
            return;
        };
        packs.name_input.update(cx, |input, cx| input.set_text(name, cx));
        packs.naming = Some(Naming::Rename);
        cx.notify();
    }

    pub(super) fn confirm_naming(&mut self, cx: &mut Context<Self>) {
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        let Some(naming) = packs.naming.take() else {
            return;
        };
        let name = packs.name_input.read(cx).text().trim().to_owned();
        match naming {
            Naming::New => {
                let title = self.target.title.clone();
                self.packs_task(
                    Text::new("mods.packs.creating"),
                    move |mods, account| {
                        let (pack, notes) =
                            super::create::from_installed(mods, account, name, &title).map_err(Text::verbatim)?;
                        pack.write(&decanter_modpack::pack_file(mods.dir(), &pack.id))
                            .map_err(save_failed)?;
                        notes
                            .write(&decanter_modpack::local_file(mods.dir(), &pack.id))
                            .map_err(save_failed)?;
                        open_data(mods, pack)
                    },
                    |this, open, cx| {
                        if let Some(packs) = this.packs.as_mut() {
                            packs.notice = Some(
                                Text::new("mods.packs.created")
                                    .arg("name", &open.pack.name)
                                    .arg("count", open.pack.entries.len()),
                            );
                            packs.open = Some(open);
                        }
                        cx.notify();
                    },
                    cx,
                );
            }
            Naming::Rename => self.edit_pack(move |pack| pack.name = name, cx),
        }
    }

    /// Changes a copy of the open pack and saves it as the next revision.
    fn edit_pack(&mut self, change: impl FnOnce(&mut Pack), cx: &mut Context<Self>) {
        let Some(open) = self.packs.as_ref().and_then(|packs| packs.open.as_ref()) else {
            return;
        };
        let mut pack = open.pack.clone();
        change(&mut pack);
        self.save_pack(pack, cx);
    }

    fn save_pack(&mut self, mut pack: Pack, cx: &mut Context<Self>) {
        pack.revision = pack.revision.saturating_add(1);
        self.packs_task(
            Text::new("mods.packs.saving"),
            move |mods, _| {
                pack.write(&decanter_modpack::pack_file(mods.dir(), &pack.id))
                    .map_err(save_failed)?;
                tracing::info!(pack = %pack.id, revision = pack.revision, "mod pack saved");
                open_data(mods, pack)
            },
            |this, open, cx| {
                if let Some(packs) = this.packs.as_mut() {
                    packs.open = Some(open);
                }
                cx.notify();
            },
            cx,
        );
    }

    pub(super) fn toggle_entry(&mut self, key: EntryKey, cx: &mut Context<Self>) {
        self.edit_pack(
            move |pack| {
                if let Some(entry) = pack.entries.iter_mut().find(|entry| entry.key == key) {
                    entry.enabled = !entry.enabled;
                    entry.is_enabled_stated = true;
                }
            },
            cx,
        );
    }

    pub(super) fn switch_track(&mut self, key: EntryKey, cx: &mut Context<Self>) {
        self.edit_pack(
            move |pack| {
                if let Some(entry) = pack.entries.iter_mut().find(|entry| entry.key == key) {
                    entry.track = if entry.track == Track::Latest {
                        Track::Pinned
                    } else {
                        Track::Latest
                    };
                }
            },
            cx,
        );
    }

    /// The entry goes, and with it every edge naming it, since the format refuses a reference to
    /// no entry.
    pub(super) fn remove_entry(&mut self, key: EntryKey, cx: &mut Context<Self>) {
        self.edit_pack(
            move |pack| {
                pack.entries.retain(|entry| entry.key != key);
                for entry in &mut pack.entries {
                    entry.requires.retain(|other| *other != key);
                    entry.after.retain(|other| *other != key);
                }
            },
            cx,
        );
    }

    /// Swaps the entry with its neighbour in the pack's order, refused when that breaks a hard
    /// edge, and asked first when it changes which installed mod wins a file or a DLL.
    pub(super) fn request_entry_move(&mut self, key: EntryKey, earlier: bool, cx: &mut Context<Self>) {
        let Some(open) = self.packs.as_ref().and_then(|packs| packs.open.as_ref()) else {
            return;
        };
        let Some(moved) = swapped(&open.pack, &key, earlier) else {
            return;
        };
        let refused = match decanter_modpack::resolve(&moved, &[]) {
            Ok(resolution) => resolution.contradictions().first().map(|broken| {
                let name_of = |key: &EntryKey| {
                    moved
                        .entry(key)
                        .and_then(|entry| entry.name.clone())
                        .unwrap_or_else(|| key.to_string())
                };
                tracing::info!(entry = %key, "pack entry move refused, it breaks a requirement");
                Text::new("mods.packs.move_refused")
                    .arg("dependant", name_of(&broken.dependant))
                    .arg("required", name_of(&broken.required))
            }),
            Err(err) => Some(Text::verbatim(err)),
        };
        if let Some(text) = refused {
            if let Some(packs) = self.packs.as_mut() {
                packs.set_error(text);
            }
            cx.notify();
            return;
        }
        let members = present_members(&moved, open);
        self.packs_task(
            Text::new("mods.order.checking"),
            move |mods, account| run::reorder_change(mods, account, &members).map_err(Text::verbatim),
            move |this, change, cx| {
                if change.is_empty() {
                    this.save_pack(moved, cx);
                } else if let Some(packs) = this.packs.as_mut() {
                    tracing::info!(
                        files = change.files.len(),
                        dlls = change.dlls.len(),
                        "pack entry move changes winners, asking first"
                    );
                    packs.pending_move = Some(PendingMove { pack: moved, change });
                }
                cx.notify();
            },
            cx,
        );
    }

    pub(super) fn entry_move_confirmation(&self, open: &Open, cx: &Context<Self>) -> Option<Div> {
        let pending = self.packs.as_ref()?.pending_move.as_ref()?;
        let name_of = |id: &ModId| open.names.get(id).cloned().unwrap_or_else(|| id.to_string());
        Some(confirm_panel(
            "pack-move-confirm",
            t!("mods.packs.move_title"),
            change_lines(&pending.change, &name_of).join("\n"),
            t!("mods.order.confirm"),
            cx.listener(|this, _, _, cx| {
                if let Some(pending) = this.packs.as_mut().and_then(|packs| packs.pending_move.take()) {
                    this.save_pack(pending.pack, cx);
                }
            }),
            cx.listener(|this, _, _, cx| {
                if let Some(packs) = this.packs.as_mut() {
                    packs.pending_move = None;
                }
                cx.notify();
            }),
        ))
    }
}

/// The pack with `key` swapped with its neighbour in the pack's order; `None` at either end.
fn swapped(pack: &Pack, key: &EntryKey, earlier: bool) -> Option<Pack> {
    let mut sorted: Vec<(u32, &EntryKey)> = pack.entries.iter().map(|entry| (entry.order, &entry.key)).collect();
    sorted.sort();
    let index = sorted.iter().position(|(_, other)| *other == key)?;
    let neighbour = if earlier {
        index.checked_sub(1)?
    } else {
        index.checked_add(1)?
    };
    let (other_order, other) = sorted.get(neighbour).map(|&(order, other)| (order, other.clone()))?;
    let (own_order, _) = *sorted.get(index)?;
    let mut moved = pack.clone();
    for entry in &mut moved.entries {
        if entry.key == *key {
            entry.order = other_order;
        } else if entry.key == other {
            entry.order = own_order;
        }
    }
    Some(moved)
}

/// The installed mods of the pack's entries, in its resolved order, as applying it would place them.
fn present_members(pack: &Pack, open: &Open) -> Vec<ModId> {
    let sequence: Vec<EntryKey> = decanter_modpack::resolve(pack, &[]).map_or_else(
        |_| {
            let mut entries: Vec<_> = pack.entries.iter().collect();
            entries.sort_by_key(|entry| entry.order);
            entries.into_iter().map(|entry| entry.key.clone()).collect()
        },
        |resolution| resolution.sequence().to_vec(),
    );
    sequence
        .iter()
        .filter_map(|key| match open.statuses.get(key)? {
            decanter_modpack::Status::Present { id, .. } | decanter_modpack::Status::Outdated { id, .. } => {
                Some(id.clone())
            }
            _ => None,
        })
        .collect()
}

/// The lines a winner-change confirmation lists, as the mod list's move confirmation words them.
pub(in crate::views::library::detail::mods) fn change_lines(
    change: &OrderChange,
    name_of: &dyn Fn(&ModId) -> String,
) -> Vec<String> {
    let mut lines = Vec::with_capacity(SHOWN_CHANGES.saturating_mul(2).saturating_add(4));
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
    lines
}

fn save_failed(err: decanter_modpack::Error) -> Text {
    Text::new("mods.packs.save_failed").arg("reason", err)
}
