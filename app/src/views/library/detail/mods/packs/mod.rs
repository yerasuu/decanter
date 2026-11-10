//! Mod packs in the Mods tab (docs/mod-packs-and-search-design.md, sections 4 to 6): the game's
//! packs, a new one made from the installed mods, its entries edited, and applying it. A pack is a
//! recipe in `packs/` beside the mods; every pack file read from there goes through `Pack::parse`,
//! since anything may have written it, and every write is read back through the same checks.

mod apply;
mod create;
mod edit;
mod remove;
mod run;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use decanter_modpack::{Entry, EntryKey, LocalNotes, Pack, PackId, Source, Status, Track};
use decanter_mods::{GameMods, ModId};
use gpui::{Context, Div, Entity, SharedString, div, prelude::*, px};
use rust_i18n::t;

use super::ModsTab;
use super::credits::{Platform, source};
use crate::i18n::Text;
use crate::theme;
use crate::widgets::input::TextInput;
use crate::widgets::{ButtonKind, button, checkbox, tooltip};

/// `decanter_modpack::pack_file`'s folder, listed here since the crate names one pack's file only.
const PACKS_DIR: &str = "packs";
/// Pack files a game's folder is read for; more are left out with a warning.
const MAX_PACKS: usize = 200;
/// The pack list's column; the selected pack takes the rest of the tab's width.
const LIST_WIDTH: f32 = 320.;

pub(super) struct Packs {
    listed: Option<Vec<Listed>>,
    open: Option<Open>,
    name_input: Entity<TextInput>,
    naming: Option<Naming>,
    busy: Option<Text>,
    error: Option<Text>,
    notice: Option<Text>,
    pending_move: Option<edit::PendingMove>,
    removal: Option<remove::Removal>,
    apply: Option<apply::Apply>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Naming {
    New,
    Rename,
}

struct Listed {
    id: PackId,
    read: Result<Pack, Text>,
}

/// The pack being edited, with what the game has of each entry.
struct Open {
    pack: Pack,
    statuses: HashMap<EntryKey, Status>,
    names: HashMap<ModId, String>,
}

impl Packs {
    pub(super) fn is_busy(&self) -> bool {
        self.busy.is_some() || self.apply.as_ref().is_some_and(apply::Apply::is_working)
    }

    /// Stops an apply in flight; the step it was on cleans up after itself.
    pub(super) fn discard(self) {
        if let Some(apply) = self.apply {
            apply.cancel();
        }
    }

    fn set_error(&mut self, text: Text) {
        self.error = Some(text);
    }
}

impl ModsTab {
    /// Readies the Packs section; it keeps its state while the tab lives, so switching back to it
    /// finds the pack and plan where they were.
    pub(super) fn show_packs(&mut self, cx: &mut Context<Self>) {
        if self.packs.is_some() {
            return;
        }
        let name_input = cx.new(|cx| TextInput::new(Text::new("mods.packs.name"), cx));
        self.packs = Some(Packs {
            listed: None,
            open: None,
            name_input,
            naming: None,
            busy: None,
            error: None,
            notice: None,
            pending_move: None,
            removal: None,
            apply: None,
        });
        tracing::info!("mod packs section opened");
        self.load_packs(cx);
        cx.notify();
    }

    fn load_packs(&self, cx: &Context<Self>) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let read = tokio::task::spawn_blocking(move || list_packs(mods.dir())).await;
            let _ = this.update(cx, |this, cx| {
                let Some(packs) = this.packs.as_mut() else {
                    return;
                };
                match read {
                    Ok(Ok(listed)) => packs.listed = Some(listed),
                    Ok(Err(text)) => {
                        tracing::warn!(error = %text, "the game's mod packs could not be listed");
                        packs.listed = Some(Vec::new());
                        packs.set_error(text);
                    }
                    Err(err) => {
                        tracing::error!(%err, "listing the game's mod packs stopped");
                        packs.set_error(Text::verbatim(err));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs `work` off the UI with the packs' actions disabled meanwhile, hands its result to
    /// `done`, then reads the mods and the packs again, since most actions change one or both.
    fn packs_task<T: Send + 'static>(
        &mut self,
        busy: Text,
        work: impl FnOnce(&GameMods, &str) -> Result<T, Text> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(mods) = self.mods.clone() else {
            return;
        };
        let Some(packs) = self.packs.as_mut() else {
            return;
        };
        packs.busy = Some(busy);
        packs.error = None;
        packs.notice = None;
        let account = self.target.account_id.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let outcome = tokio::task::spawn_blocking(move || work(&mods, &account)).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(packs) = this.packs.as_mut() {
                    packs.busy = None;
                }
                match outcome {
                    Ok(Ok(value)) => done(this, value, cx),
                    Ok(Err(text)) => {
                        tracing::warn!(error = %text, "mod pack action failed");
                        if let Some(packs) = this.packs.as_mut() {
                            packs.set_error(text);
                        }
                    }
                    Err(err) => {
                        tracing::error!(%err, "mod pack action stopped");
                        if let Some(packs) = this.packs.as_mut() {
                            packs.set_error(Text::verbatim(err));
                        }
                    }
                }
                this.reload(cx);
                this.load_packs(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn open_pack(&mut self, id: PackId, cx: &mut Context<Self>) {
        self.packs_task(
            Text::new("mods.packs.reading"),
            move |mods, _| {
                let pack = Pack::read(&decanter_modpack::pack_file(mods.dir(), &id)).map_err(read_failed)?;
                open_data(mods, pack)
            },
            |this, open, cx| {
                if let Some(packs) = this.packs.as_mut() {
                    // A plan shown is of the pack that was open, never of the one chosen now.
                    if !packs.apply.as_ref().is_some_and(apply::Apply::is_working) {
                        packs.apply = None;
                    }
                    packs.open = Some(open);
                }
                cx.notify();
            },
            cx,
        );
    }

    /// The Packs section of the Mods tab, at the tab's full width: the game's packs with create,
    /// rename and remove on the left, the selected pack's entries or its apply plan and progress on
    /// the right. `show_packs` first, when the section is chosen; empty until then.
    pub(super) fn packs_section(&self, locked: bool, cx: &Context<Self>) -> Div {
        let Some(packs) = self.packs.as_ref() else {
            return div();
        };
        let own_lock = locked || packs.naming.is_some() || packs.pending_move.is_some() || packs.removal.is_some();
        let selected = packs.open.as_ref().map(|open| &open.pack.id);
        let right = if let Some(apply) = packs.apply.as_ref() {
            Self::apply_panel(apply, locked, cx)
        } else if let Some(open) = packs.open.as_ref() {
            self.editor(open, own_lock, cx)
        } else {
            theme::muted(t!("mods.packs.choose"))
        };
        let left = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(LIST_WIDTH))
            .flex_none()
            .child(theme::h3(t!("mods.packs.title")))
            .child(theme::muted(t!("mods.packs.note")).text_size(px(12.5)))
            .children(Self::naming_form(packs, cx))
            .children(self.removal_panel(cx))
            .child(Self::pack_list(packs, selected, own_lock, cx));
        div()
            .flex()
            .flex_col()
            .gap_3()
            .w_full()
            .children(packs.busy.as_ref().map(theme::muted))
            .children(packs.notice.as_ref().map(theme::muted))
            .children(packs.error.as_ref().map(theme::error))
            .child(
                div().flex().items_start().gap_6().w_full().child(left).child(
                    theme::card()
                        .flex_1()
                        .min_w_0()
                        .gap_3()
                        .px(px(18.))
                        .py(px(14.))
                        .child(right),
                ),
            )
    }

    fn naming_form(packs: &Packs, cx: &Context<Self>) -> Option<Div> {
        let naming = packs.naming?;
        let can_save = !packs.name_input.read(cx).text().trim().is_empty() && packs.busy.is_none();
        let label = match naming {
            Naming::New => t!("mods.packs.create"),
            Naming::Rename => t!("mods.packs.rename_save"),
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(theme::muted(match naming {
                    Naming::New => t!("mods.packs.create_note"),
                    Naming::Rename => t!("mods.packs.rename_note"),
                }))
                .child(div().max_w(px(420.)).child(packs.name_input.clone()))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            button("pack-name-save", ButtonKind::Primary, label, !can_save).when(can_save, |b| {
                                b.on_click(cx.listener(|this, _, _, cx| this.confirm_naming(cx)))
                            }),
                        )
                        .child(
                            button("pack-name-cancel", ButtonKind::Secondary, t!("common.cancel"), false).on_click(
                                cx.listener(|this, _, _, cx| {
                                    if let Some(packs) = this.packs.as_mut() {
                                        packs.naming = None;
                                    }
                                    cx.notify();
                                }),
                            ),
                        ),
                ),
        )
    }

    fn pack_list(packs: &Packs, selected: Option<&PackId>, locked: bool, cx: &Context<Self>) -> Div {
        let disabled = locked || packs.is_busy();
        let create = button(
            "pack-new",
            ButtonKind::Secondary,
            t!("mods.packs.new_from_installed"),
            disabled,
        )
        .when(!disabled, |b| {
            b.on_click(cx.listener(|this, _, _, cx| this.start_new_pack(cx)))
        });
        let list = match packs.listed.as_ref() {
            None => theme::muted(t!("mods.packs.loading")),
            Some(listed) if listed.is_empty() => theme::muted(t!("mods.packs.none")),
            Some(listed) => div().flex().flex_col().gap_2().children(
                listed
                    .iter()
                    .enumerate()
                    .map(|(index, listed)| pack_row(index, listed, selected == Some(&listed.id), disabled, cx)),
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(list)
            .child(div().flex().child(create))
    }

    fn editor(&self, open: &Open, locked: bool, cx: &Context<Self>) -> Div {
        let packs = self.packs.as_ref();
        let disabled = locked || packs.is_some_and(Packs::is_busy);
        let pack = &open.pack;
        let mut entries: Vec<&Entry> = pack.entries.iter().collect();
        entries.sort_by_key(|entry| entry.order);
        let count = entries.len();
        let id = pack.id.clone();
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(10.))
            .child(
                button(
                    "pack-apply",
                    ButtonKind::Primary,
                    t!("mods.packs.apply"),
                    disabled || count == 0,
                )
                .when(!disabled && count > 0, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.start_apply(id.clone(), cx)))
                }),
            )
            .child(
                button("pack-rename", ButtonKind::Secondary, t!("mods.packs.rename"), disabled).when(!disabled, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.start_rename(cx)))
                }),
            );
        let rows = entries.iter().enumerate().map(|(index, entry)| {
            entry_row(
                index,
                entry,
                open,
                index == 0,
                index.saturating_add(1) == count,
                disabled,
                cx,
            )
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().child(pack.name.clone()))
            .child(theme::muted(t!(
                "mods.packs.summary",
                revision = pack.revision,
                count = count
            )))
            .child(actions)
            .children(self.entry_move_confirmation(open, cx))
            .child(theme::card().children(rows).when(count == 0, |card| {
                card.child(theme::muted(t!("mods.packs.empty")).p_3())
            }))
            .child(theme::muted(t!("mods.packs.order_note")).text_size(px(12.5)))
    }
}

fn pack_row(index: usize, listed: &Listed, is_selected: bool, disabled: bool, cx: &Context<ModsTab>) -> Div {
    let id = listed.id.clone();
    let remove_id = listed.id.clone();
    let row = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_3()
        .py_1()
        .px_2()
        .border_b_1()
        .border_color(gpui::rgb(theme::LINE))
        .when(is_selected, |row| row.bg(gpui::rgb(theme::BG_PANEL_3)));
    match &listed.read {
        Ok(pack) => {
            let name = pack.name.clone();
            row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(pack.name.clone())
                    .child(
                        theme::muted(t!(
                            "mods.packs.summary",
                            revision = pack.revision,
                            count = pack.entries.len()
                        ))
                        .text_size(px(12.5)),
                    ),
            )
            .child(
                button(
                    ("pack-open", index),
                    ButtonKind::Secondary,
                    t!("mods.packs.open"),
                    disabled,
                )
                .when(!disabled, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.open_pack(id.clone(), cx)))
                }),
            )
            .child(
                button(
                    ("pack-remove", index),
                    ButtonKind::Secondary,
                    t!("mods.packs.remove"),
                    disabled,
                )
                .when(!disabled, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.request_remove(remove_id.clone(), name.clone(), cx);
                    }))
                }),
            )
        }
        Err(text) => row
            .child(div().flex_1().min_w_0().child(theme::error(
                Text::new("mods.packs.unreadable").text("reason", text.clone()),
            )))
            .child(
                button(
                    ("pack-remove", index),
                    ButtonKind::Secondary,
                    t!("mods.packs.remove"),
                    disabled,
                )
                .when(!disabled, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| {
                        this.request_remove(remove_id.clone(), remove_id.to_string(), cx);
                    }))
                }),
            ),
    }
}

fn entry_row(
    index: usize,
    entry: &Entry,
    open: &Open,
    first: bool,
    last: bool,
    disabled: bool,
    cx: &Context<ModsTab>,
) -> Div {
    let (toggle_key, up_key, down_key, track_key, remove_key) = (
        entry.key.clone(),
        entry.key.clone(),
        entry.key.clone(),
        entry.key.clone(),
        entry.key.clone(),
    );
    let enabled = entry.enabled;
    let status = open.statuses.get(&entry.key);
    let status_text = match status {
        Some(Status::Present { .. }) => t!("mods.packs.status_present"),
        Some(Status::Outdated { .. }) => t!("mods.packs.status_outdated"),
        _ => t!("mods.packs.status_missing"),
    };
    let is_local = matches!(entry.source, Source::Local { .. });
    let track_label = if entry.track == Track::Latest {
        t!("mods.packs.track_latest")
    } else {
        t!("mods.packs.track_pinned")
    };
    let can_track = !disabled && !is_local;
    let header = div()
        .flex()
        .items_center()
        .gap_2()
        .child(theme::muted(t!("mods.order.slot", slot = index.saturating_add(1))).min_w(px(24.)))
        .child(
            div()
                .id(("pack-entry-toggle", index))
                .when(!disabled, |toggle| {
                    toggle.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_entry(toggle_key.clone(), cx);
                    }))
                })
                .child(checkbox(enabled).when(disabled, |b| b.opacity(0.4))),
        )
        .child(
            button(("pack-entry-up", index), ButtonKind::Icon, "↑", disabled || first)
                .tooltip(tooltip(t!("mods.order.earlier")))
                .when(!disabled && !first, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.request_entry_move(up_key.clone(), true, cx)))
                }),
        )
        .child(
            button(("pack-entry-down", index), ButtonKind::Icon, "↓", disabled || last)
                .tooltip(tooltip(t!("mods.order.later")))
                .when(!disabled && !last, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.request_entry_move(down_key.clone(), false, cx)))
                }),
        )
        .child(div().flex_1().min_w_0().child(entry_name(entry)))
        .child(
            button(("pack-entry-remove", index), ButtonKind::Icon, "×", disabled)
                .tooltip(tooltip(t!("mods.packs.remove_entry")))
                .when(!disabled, |b| {
                    b.on_click(cx.listener(move |this, _, _, cx| this.remove_entry(remove_key.clone(), cx)))
                }),
        );
    let details = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .pl(px(29.))
        .text_size(px(12.5))
        .child(attribution(entry))
        .child(theme::muted(version_text(entry)))
        .children((!is_local).then(|| {
            button(
                ("pack-entry-track", index),
                ButtonKind::Secondary,
                track_label,
                !can_track,
            )
            .tooltip(tooltip(t!("mods.packs.track_tip")))
            .when(can_track, |b| {
                b.on_click(cx.listener(move |this, _, _, cx| this.switch_track(track_key.clone(), cx)))
            })
        }))
        .child(theme::muted(status_text))
        .children((!entry.is_shareable()).then(|| theme::muted(t!("mods.packs.not_shareable"))));
    div()
        .flex()
        .flex_col()
        .gap_1()
        .px(px(12.))
        .py(px(8.))
        .when(!enabled, |row| row.opacity(0.6))
        .when(!last, |row| row.border_b_1().border_color(gpui::rgb(theme::LINE)))
        .child(header)
        .child(details)
}

/// The entry's name, as the pack keeps it for display, marked when a dependency brought it.
fn entry_name(entry: &Entry) -> String {
    let name = entry.name.clone().unwrap_or_else(|| entry.key.to_string());
    if entry.auto {
        t!("mods.packs.auto", name = name).into_owned()
    } else {
        name
    }
}

/// Where the entry comes from and who made it, as the credits under an installed mod say it.
fn attribution(entry: &Entry) -> Div {
    let platform = platform_of(&entry.source).map_or_else(chip_local, source);
    div().flex().items_center().gap_2().child(platform).children(
        entry
            .author
            .as_deref()
            .map(|author| div().child(t!("mods.credits.by", author = author))),
    )
}

fn chip_local() -> Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_color(gpui::rgb(theme::LINE))
        .text_size(px(11.5))
        .text_color(gpui::rgb(theme::TEXT_MUTED))
        .child(SharedString::from(t!("mods.packs.local").into_owned()))
}

const fn platform_of(source: &Source) -> Option<Platform> {
    match source {
        Source::Thunderstore { .. } => Some(Platform::Thunderstore),
        Source::Nexus { .. } => Some(Platform::Nexus),
        Source::Modio { .. } => Some(Platform::Modio),
        _ => None,
    }
}

fn version_text(entry: &Entry) -> String {
    match &entry.source {
        Source::Thunderstore { version, .. } => version.clone(),
        Source::Nexus { version, file, .. } | Source::Modio { version, file, .. } => version
            .clone()
            .unwrap_or_else(|| t!("mods.packs.file_id", id = file).into_owned()),
        Source::Local { file_name } => file_name.clone(),
        _ => String::new(),
    }
}

fn read_failed(err: decanter_modpack::Error) -> Text {
    Text::new("mods.packs.unreadable").arg("reason", err)
}

/// Every pack file of the game's `packs/`, each read through the format's checks; one that does not
/// hold up is listed with why, so it can still be removed.
fn list_packs(game_dir: &Path) -> Result<Vec<Listed>, Text> {
    let folder = game_dir.join(PACKS_DIR);
    let entries = match fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Text::verbatim(format!("{}: {err}", folder.display()))),
    };
    let mut listed = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| Text::verbatim(format!("{}: {err}", folder.display())))?;
        let file_name = entry.file_name();
        let Some(stem) = file_name.to_str().and_then(|name| name.strip_suffix(".toml")) else {
            continue;
        };
        let Ok(id) = stem.parse::<PackId>() else {
            continue;
        };
        if listed.len() >= MAX_PACKS {
            tracing::warn!(cap = MAX_PACKS, "more mod packs than are listed, the rest left out");
            break;
        }
        let read = Pack::read(&entry.path()).map_err(read_failed).and_then(|pack| {
            if pack.id == id {
                Ok(pack)
            } else {
                Err(Text::new("mods.packs.id_mismatch"))
            }
        });
        if let Err(text) = &read {
            tracing::warn!(pack = %id, error = %text, "mod pack file refused");
        }
        listed.push(Listed { id, read });
    }
    listed.sort_by(|left, right| match (&left.read, &right.read) {
        (Ok(left), Ok(right)) => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
        (Ok(_), Err(_)) => core::cmp::Ordering::Less,
        (Err(_), Ok(_)) => core::cmp::Ordering::Greater,
        (Err(_), Err(_)) => left.id.as_str().cmp(right.id.as_str()),
    });
    Ok(listed)
}

/// The pack with what the game has of each entry, read off the UI.
fn open_data(mods: &GameMods, pack: Pack) -> Result<Open, Text> {
    let notes = read_notes(mods, &pack.id);
    let installed = mods.list().map_err(Text::verbatim)?;
    let diff = decanter_modpack::diff(&pack, &installed, notes.as_ref());
    let statuses = diff
        .entries
        .into_iter()
        .map(|entry| (entry.key, entry.status))
        .collect();
    let names = installed
        .into_iter()
        .map(|manifest| (manifest.id, manifest.name))
        .collect();
    Ok(Open { pack, statuses, names })
}

/// The pack's notes; an unreadable file counts as none, which only makes every mod adopted.
fn read_notes(mods: &GameMods, id: &PackId) -> Option<LocalNotes> {
    LocalNotes::read(&decanter_modpack::local_file(mods.dir(), id))
        .inspect_err(|err| tracing::warn!(pack = %id, %err, "mod pack notes unreadable, every mod counts as adopted"))
        .ok()
        .flatten()
        .filter(|notes| notes.pack() == id)
}
