//! Removing a pack (design, section 6.6): its file and its notes go and every mod stays, unless the
//! user also ticks its mods, which offers only the mods this pack's apply installed, never one it
//! adopted, one another pack's notes name, or one another account has on. Without notes every mod
//! counts as adopted, so nothing is offered.

use std::collections::HashSet;
use std::fs;
use std::io;

use decanter_modpack::{Pack, PackId};
use decanter_mods::{GameMods, ModId};
use gpui::{Context, Div, div, prelude::*, px};
use rust_i18n::t;

use super::{PACKS_DIR, read_notes};
use crate::i18n::Text;
use crate::theme;
use crate::views::library::detail::mods::ModsTab;
use crate::widgets::{ButtonKind, button, checkbox};

/// How many of the mods a removal takes it names before summing up the rest.
const SHOWN_MODS: usize = 12;

pub(super) struct Removal {
    id: PackId,
    name: String,
    /// The mods the pack installed that it may take, with their names and sizes.
    mods: Vec<(ModId, String, u64)>,
    /// Mods the pack installed that stay, since another pack or account uses them.
    kept: usize,
    with_mods: bool,
}

impl ModsTab {
    pub(super) fn request_remove(&mut self, id: PackId, name: String, cx: &mut Context<Self>) {
        self.packs_task(
            Text::new("mods.packs.reading"),
            move |mods, account| removable(mods, account, &id).map(|(offered, kept)| (id, name, offered, kept)),
            |this, (id, name, mods, kept), cx| {
                if let Some(packs) = this.packs.as_mut() {
                    packs.removal = Some(Removal {
                        id,
                        name,
                        mods,
                        kept,
                        with_mods: false,
                    });
                }
                cx.notify();
            },
            cx,
        );
    }

    fn confirm_remove(&mut self, cx: &mut Context<Self>) {
        let Some(removal) = self.packs.as_mut().and_then(|packs| packs.removal.take()) else {
            return;
        };
        let doomed: Vec<ModId> = if removal.with_mods {
            removal.mods.into_iter().map(|(id, _, _)| id).collect()
        } else {
            Vec::new()
        };
        let (id, name) = (removal.id, removal.name);
        let removed_id = id.clone();
        self.packs_task(
            Text::new("mods.packs.removing"),
            move |mods, _| remove(mods, &id, &doomed),
            move |this, (removed, failed), cx| {
                if let Some(packs) = this.packs.as_mut() {
                    if packs.open.as_ref().is_some_and(|open| open.pack.id == removed_id) {
                        packs.open = None;
                    }
                    packs.notice = Some(Text::new("mods.packs.removed").arg("name", &name).arg("count", removed));
                    if failed > 0 {
                        packs.set_error(Text::new("mods.packs.remove_failed").arg("count", failed));
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    pub(super) fn removal_panel(&self, cx: &Context<Self>) -> Option<Div> {
        let removal = self.packs.as_ref()?.removal.as_ref()?;
        let size = removal
            .mods
            .iter()
            .map(|(_, _, size)| *size)
            .fold(0_u64, u64::saturating_add);
        let with_mods = removal.with_mods;
        let rest = removal.mods.len().saturating_sub(SHOWN_MODS);
        let tick = (!removal.mods.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .id("pack-remove-mods")
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(removal) = this.packs.as_mut().and_then(|packs| packs.removal.as_mut()) {
                                removal.with_mods = !removal.with_mods;
                            }
                            cx.notify();
                        }))
                        .child(checkbox(with_mods))
                        .child(t!(
                            "mods.packs.remove_mods",
                            count = removal.mods.len(),
                            size = crate::format::bytes(size)
                        )),
                )
                .children(
                    removal
                        .mods
                        .iter()
                        .take(SHOWN_MODS)
                        .map(|(_, name, _)| theme::muted(name.clone()).pl(px(24.)).text_size(px(12.5))),
                )
                .children((rest > 0).then(|| theme::muted(t!("library.detail.mods.more", count = rest)).pl(px(24.))))
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .border_1()
                .border_color(gpui::rgb(theme::ACCENT_GOLD))
                .rounded_md()
                .child(theme::h3(t!("mods.packs.remove_title", name = removal.name)))
                .child(div().child(t!("mods.packs.remove_body")))
                .children(tick)
                .children((removal.kept > 0).then(|| theme::muted(t!("mods.packs.remove_kept", count = removal.kept))))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button(
                                "pack-remove-yes",
                                ButtonKind::Primary,
                                if with_mods {
                                    t!("mods.packs.remove_with_mods")
                                } else {
                                    t!("mods.packs.remove")
                                },
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.confirm_remove(cx))),
                        )
                        .child(
                            button("pack-remove-no", ButtonKind::Secondary, t!("common.cancel"), false).on_click(
                                cx.listener(|this, _, _, cx| {
                                    if let Some(packs) = this.packs.as_mut() {
                                        packs.removal = None;
                                    }
                                    cx.notify();
                                }),
                            ),
                        ),
                ),
        )
    }
}

/// The mods removing the pack may take, and how many it installed that stay for others.
fn removable(mods: &GameMods, account: &str, id: &PackId) -> Result<(Vec<(ModId, String, u64)>, usize), Text> {
    let Some(notes) = read_notes(mods, id) else {
        return Ok((Vec::new(), 0));
    };
    let elsewhere = other_packs_mods(mods, id);
    let installed = mods.list().map_err(Text::verbatim)?;
    let mut offered = Vec::new();
    let mut kept = 0_usize;
    for mod_id in notes.installed() {
        let Some(manifest) = installed.iter().find(|manifest| manifest.id == *mod_id) else {
            continue;
        };
        let users = mods.users(mod_id).map_err(Text::verbatim)?;
        if elsewhere.contains(mod_id) || users.iter().any(|user| user != account) {
            kept = kept.saturating_add(1);
            continue;
        }
        offered.push((manifest.id.clone(), manifest.name.clone(), manifest.size()));
    }
    Ok((offered, kept))
}

/// Every mod another pack's notes link to one of its entries, installed or adopted, which this
/// removal leaves alone.
fn other_packs_mods(mods: &GameMods, id: &PackId) -> HashSet<ModId> {
    let mut named = HashSet::new();
    let Ok(entries) = fs::read_dir(mods.dir().join(PACKS_DIR)) else {
        return named;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(other) = file_name
            .to_str()
            .and_then(|name| name.strip_suffix(".toml"))
            .and_then(|stem| stem.parse::<PackId>().ok())
            .filter(|other| other != id)
        else {
            continue;
        };
        let (pack, notes) = match (Pack::read(&entry.path()), read_notes(mods, &other)) {
            (Ok(pack), Some(notes)) => (pack, notes),
            (Err(err), _) => {
                tracing::warn!(pack = %other, %err, "another pack unreadable, its mods are not known");
                continue;
            }
            (Ok(_), None) => continue,
        };
        named.extend(
            pack.entries
                .iter()
                .filter_map(|entry| notes.get(&entry.key).map(|note| note.id.clone())),
        );
    }
    named
}

/// The mods first, then the pack's two files; a mod that can't be removed is counted and stays.
fn remove(mods: &GameMods, id: &PackId, doomed: &[ModId]) -> Result<(usize, usize), Text> {
    let mut removed = 0_usize;
    let mut failed = 0_usize;
    for mod_id in doomed {
        match mods.remove(mod_id) {
            Ok(()) => removed = removed.saturating_add(1),
            Err(err) => {
                tracing::warn!(pack = %id, mod_id = %mod_id, %err, "a mod of the pack could not be removed");
                failed = failed.saturating_add(1);
            }
        }
    }
    for path in [
        decanter_modpack::pack_file(mods.dir(), id),
        decanter_modpack::local_file(mods.dir(), id),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(Text::verbatim(format!("{}: {err}", path.display()))),
        }
    }
    tracing::info!(pack = %id, removed, failed, "mod pack removed");
    Ok((removed, failed))
}
