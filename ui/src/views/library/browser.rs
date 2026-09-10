//! The cover-art view: collections as horizontal shelves of capsules,
//! the way Steam's library home shows them.
//!
//! Mostly unfiltered — this is the "browse my shelves" view, and a
//! collection that looked half-empty because a filter was still set in
//! the sidebar would be misleading. Narrowing is what the sidebar list is
//! for; this is for looking at what you have.
//!
//! "Solo instalados" is the one exception, shared with `Sidebar` through
//! `Library`'s `filters` signal rather than owned by either: unlike
//! genre/achievement/search narrowing, which really is sidebar-specific,
//! "do I have this or not" is a property of the whole library the user
//! expects to hold everywhere, not just in one list.

use std::rc::Rc;

use achv_gog::LibraryEntry;
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::context_menu::{ContextMenu, MenuTarget};
use super::filters::{Sort, is_installed, pending_install_ids, sort_entries};
use super::{data, shelves};

/// One row of the home layout. A shelf is a slot the user points at a
/// source, not a fixed view of one collection — which is what lets the
/// same collection appear twice, ordered two different ways.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Slot {
    /// A render handle only — handed out fresh on every load, never saved.
    #[serde(skip)]
    key: u32,
    /// Shelf id from [`shelves::build`], or `"all"` for the whole library.
    source: String,
    sort: Sort,
}

/// Sources that exist no matter what the user has made, so a saved shelf
/// pointing at one is always valid.
const BUILT_IN_SOURCES: [&str; 4] = ["all", "favorites", "uncategorized", "installed"];

fn load_slots(profile_id: &str) -> Vec<Slot> {
    let collection_ids: Vec<String> = data::load_collections(profile_id)
        .list
        .into_iter()
        .map(|collection| collection.id)
        .collect();
    restore_slots(
        data::view_state(profile_id, data::BROWSER_SHELVES).as_deref(),
        &collection_ids,
    )
}

/// The saved layout, with any shelf whose collection has since been
/// deleted dropped: a stale source renders as a permanently empty shelf
/// whose own name is missing from the picker, with no way to tell what it
/// was meant to be.
///
/// Unreadable JSON (hand-edited, or written by a much newer build) is
/// treated as nothing saved, the same as a first run.
fn restore_slots(raw: Option<&str>, collection_ids: &[String]) -> Vec<Slot> {
    let mut slots: Vec<Slot> = raw
        .and_then(|raw| serde_json::from_str::<Vec<Slot>>(raw).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|slot| {
            BUILT_IN_SOURCES.contains(&slot.source.as_str()) || collection_ids.iter().any(|id| id == &slot.source)
        })
        .collect();
    // Opens on the whole library: it's the one source that always exists,
    // where "favorites" is absent until something is starred and would
    // leave the first shelf pointing at nothing.
    if slots.is_empty() {
        slots.push(Slot {
            key: 0,
            source: "all".to_string(),
            sort: Sort::RecentlyPlayed,
        });
    }
    // Keys are handed out here, not restored: they only have to be unique
    // within this mount, and `next_key` continues from the count.
    for (index, slot) in slots.iter_mut().enumerate() {
        slot.key = index as u32;
    }
    slots
}

fn save_slots(profile_id: &str, slots: &[Slot]) {
    if let Some(encoded) = save_slots_encode(slots) {
        data::save_view_state(profile_id, data::BROWSER_SHELVES, &encoded);
    }
}

fn save_slots_encode(slots: &[Slot]) -> Option<String> {
    serde_json::to_string(slots).ok()
}

#[component]
pub(super) fn ShelfBrowser(
    games: Signal<Vec<LibraryEntry>>,
    profile_id: String,
    selected: Signal<Option<String>>,
    /// Shared with `Sidebar`: a capsule's right-click menu and a sidebar
    /// row's are the same popup, so only one can be open across the whole
    /// `Library` view at once.
    mut popup: Signal<Option<super::LibraryPopup>>,
    /// Also shared with `Sidebar`, but only consulted for its
    /// "solo instalados" state: unlike search/genre/achievement filters,
    /// which stay scoped to the sidebar list, that one condition narrows
    /// every shelf in this view too.
    filters: Signal<super::filters::Filters>,
) -> Element {
    let background = use_context::<crate::background::Background>();
    let generation = background.organisation;
    let mut slots = use_signal(Vec::<Slot>::new);
    let mut next_key = use_signal(|| 0_u32);
    // Restored once per profile, not per render — re-reading the saved
    // layout every render would undo a drag the moment it landed — and
    // keyed on the profile because the navbar's switcher changes it
    // without remounting this view.
    let mut loaded_profile = use_signal(|| None::<String>);
    if loaded_profile.peek().as_deref() != Some(profile_id.as_str()) {
        let restored = load_slots(&profile_id);
        next_key.set(restored.len() as u32);
        slots.set(restored);
        loaded_profile.set(Some(profile_id.clone()));
    }
    // Written back on every change. Selecting a game unmounts this whole
    // view, so without this the arrangement wouldn't survive a click on a
    // capsule, let alone a restart. Reads the profile from the signal, not
    // the prop: an effect's closure is captured once, at mount.
    use_effect(move || {
        let Some(id) = loaded_profile() else { return };
        save_slots(&id, &slots.read());
    });
    // Which shelf is being dragged, for the header-drag reordering Steam
    // uses. Held here rather than in the drag event, whose dataTransfer
    // the webview doesn't hand back reliably.
    let mut dragging_shelf = use_signal(|| None::<u32>);
    // Which shelf the drag is currently over, so the header can show where
    // the dragged one would land.
    let mut drop_target = use_signal(|| None::<u32>);

    let _ = generation();
    let collections = data::load_collections(&profile_id);
    let favorites = data::favorite_ids(&profile_id);
    let hidden = data::hidden_ids(&profile_id);
    let stats = data::library_stats(&profile_id);
    // A queued or downloading game should already read as installed here,
    // not just once bottles-next's own record catches up.
    let pending_installs = pending_install_ids(&background.installing.read());

    // Only real games: a shelf of DLC capsules is a wall of near-identical
    // art for things that can't be launched.
    let installed_only = filters.read().states.contains(&super::filters::PlayState::Installed);
    let entries: Vec<LibraryEntry> = games
        .read()
        .iter()
        .filter(|entry| {
            entry.kind == achv_gog::GameKind::Game
                && entry.parent_id.is_none()
                && super::filters::belongs_in_the_library(entry)
        })
        .filter(|entry| !hidden.contains(&entry.game.id))
        .filter(|entry| !installed_only || is_installed(&entry.game, &pending_installs))
        .cloned()
        .collect();
    let shelves = shelves::build(&entries, &collections, &favorites, &stats, &pending_installs);
    // Built-ins first, then the user's own collections — the order the
    // design prototype settled on, so the two don't drift.
    //
    // Listed unconditionally rather than picked out of `shelves`, which
    // drops a shelf the moment it's empty: a slot pointed at Favoritos
    // used to lose its own option the moment the last star came off, and
    // Instalados vanished as soon as a first collection retired the
    // install-state split. Either left the shelf blank with no way back.
    let mut sources: Vec<(String, String)> = vec![
        ("favorites".to_string(), "Favoritos".to_string()),
        ("all".to_string(), "Toda la biblioteca".to_string()),
        ("uncategorized".to_string(), "Sin coleccion".to_string()),
        ("installed".to_string(), "Instalados".to_string()),
    ];
    sources.extend(
        shelves
            .iter()
            .filter(|shelf| shelf.collection_id.is_some())
            .map(|shelf| (shelf.id.clone(), shelf.title.clone())),
    );

    rsx! {
        div { class: "shelf-browser",
            if entries.is_empty() {
                div { class: "empty", "Todavia no hay nada que mostrar." }
            }

            for slot in slots() {
                {
                    let pick = |keep: &dyn Fn(&LibraryEntry) -> bool| {
                        entries.iter().filter(|e| keep(e)).cloned().collect::<Vec<_>>()
                    };
                    let mut items = match slot.source.as_str() {
                        "all" => entries.clone(),
                        "favorites" => pick(&|e| favorites.contains(&e.game.id)),
                        "uncategorized" => pick(&|e| collections.is_uncategorized(&e.game.id)),
                        "installed" => pick(&|e| is_installed(&e.game, &pending_installs)),
                        id => shelves
                            .iter()
                            .find(|shelf| shelf.id == id)
                            .map(|shelf| shelf.entries.clone())
                            .unwrap_or_default(),
                    };
                    sort_entries(&mut items, slot.sort, &stats);
                    let installed = items
                        .iter()
                        .filter(|entry| is_installed(&entry.game, &pending_installs))
                        .count();
                    let total = items.len();
                    let only_slot = slots.read().len() == 1;
                    let (source_key, sort_key, remove_key) = (slot.key, slot.key, slot.key);

                    let is_dragging = dragging_shelf() == Some(slot.key);
                    let is_drop_target = drop_target() == Some(slot.key);
                    let is_dynamic = shelves
                        .iter()
                        .any(|shelf| shelf.id == slot.source && shelf.dynamic);

                    rsx! {
                        div {
                            class: match (is_dragging, is_drop_target) {
                                (true, _) => "browser-shelf dragging",
                                (false, true) => "browser-shelf drop-target",
                                _ => "browser-shelf",
                            },
                            key: "{slot.key}",
                            div {
                                class: "browser-shelf-head",
                                // Steam reorders shelves by dragging the header
                                // itself — the whole bar is the handle.
                                draggable: true,
                                ondragstart: move |_| dragging_shelf.set(Some(source_key)),
                                ondragend: move |_| {
                                    dragging_shelf.set(None);
                                    drop_target.set(None);
                                },
                                ondragover: move |event| {
                                    if dragging_shelf().is_some_and(|k| k != source_key) {
                                        event.prevent_default();
                                        drop_target.set(Some(source_key));
                                    }
                                },
                                ondragleave: move |_| {
                                    if drop_target() == Some(source_key) {
                                        drop_target.set(None);
                                    }
                                },
                                ondrop: move |event| {
                                    event.prevent_default();
                                    drop_target.set(None);
                                    let Some(from_key) = dragging_shelf() else { return };
                                    let mut all = slots.write();
                                    let from = all.iter().position(|s| s.key == from_key);
                                    let to = all.iter().position(|s| s.key == source_key);
                                    if let (Some(from), Some(to)) = (from, to)
                                        && from != to
                                    {
                                        let moved = all.remove(from);
                                        all.insert(to, moved);
                                    }
                                    dragging_shelf.set(None);
                                },
                                span { class: "shelf-grip", title: "Arrastrar para reordenar", "⠿" }
                                // The source picker *is* the shelf's title.
                                select {
                                    class: "shelf-source",
                                    value: "{slot.source}",
                                    onchange: move |event| {
                                        let value = event.value();
                                        let mut all = slots.write();
                                        if let Some(found) = all.iter_mut().find(|s| s.key == source_key) {
                                            found.source = value;
                                        }
                                    },
                                    for (id, label) in sources.clone() {
                                        option { key: "{id}", value: "{id}", "{label}" }
                                    }
                                }
                                if is_dynamic {
                                    span { class: "auto-tag", "AUTO" }
                                }
                                select {
                                    class: "shelf-sort",
                                    value: "{slot.sort.value()}",
                                    onchange: move |event| {
                                        let value = Sort::from_value(&event.value());
                                        let mut all = slots.write();
                                        if let Some(found) = all.iter_mut().find(|s| s.key == sort_key) {
                                            found.sort = value;
                                        }
                                    },
                                    option { value: "title", "Nombre" }
                                    option { value: "played", "Mas jugado" }
                                    option { value: "recent", "Jugado recientemente" }
                                    option { value: "completion", "Logros" }
                                    option { value: "release", "Lanzamiento" }
                                    option { value: "size", "Tamano en disco" }
                                    option { value: "rating", "Calificacion" }
                                    option { value: "added", "Fecha agregada" }
                                }
                                span { class: "shelf-count", "{installed}/{total} instalados" }
                                button {
                                    class: "icon-btn shelf-remove",
                                    title: "Quitar estanteria",
                                    disabled: only_slot,
                                    onclick: move |_| {
                                        slots.write().retain(|s| s.key != remove_key);
                                    },
                                    "🗑"
                                }
                            }

                            if items.is_empty() {
                                div { class: "shelf-empty", "Nada en esta estanteria." }
                            } else {
                                ShelfRow {
                                    items,
                                    stats: stats.clone(),
                                    favorites: favorites.clone(),
                                    pending_installs: pending_installs.clone(),
                                    selected,
                                    popup,
                                }
                            }
                        }
                    }
                }
            }

            button {
                class: "add-shelf",
                onclick: move |_| {
                    let key = next_key();
                    next_key.set(key + 1);
                    slots.write().push(Slot { key, source: "all".to_string(), sort: Sort::Title });
                },
                "+ Anadir estanteria"
            }

            if let Some(super::LibraryPopup::GameMenu(target)) = popup() {
                ContextMenu {
                    target,
                    profile_id: profile_id,
                    collections: collections,
                    generation,
                    games,
                    on_close: move |()| popup.set(None),
                }
            }
        }
    }
}

/// One shelf's scrolling row of capsules.
///
/// Its own component because the arrows need the row's scroll position, and
/// a signal per row can't be created inside the `for` that lays the shelves
/// out.
///
/// One row that scrolls sideways rather than a wrapping grid: it keeps each
/// shelf to a predictable height so several are visible at once. The
/// scrollbar is hidden and paged by the edge arrows, as Steam does it.
#[component]
fn ShelfRow(
    items: Vec<LibraryEntry>,
    /// Passed down whole: the capsules' meta lines come from it, and
    /// re-reading sqlite once per capsule would be hundreds of queries.
    stats: data::LibraryStats,
    favorites: std::collections::HashSet<String>,
    pending_installs: std::collections::HashSet<String>,
    selected: Signal<Option<String>>,
    popup: Signal<Option<super::LibraryPopup>>,
) -> Element {
    let mut row = use_signal(|| None::<Rc<MountedData>>);
    let mut reach = use_signal(RowReach::default);

    // Re-measured whenever the shelf's contents change: a row that just
    // lost half its capsules may now fit, and its arrows should say so.
    use_effect(use_reactive!(|items| {
        if let Some(node) = row() {
            spawn(async move {
                if let Some(measured) = measure(&node).await {
                    reach.set(measured);
                }
            });
        }
        let _ = items;
    }));

    // Both arrows share this: measure, then let the result decide whether
    // either still has anywhere to go.
    let remeasure = move || {
        if let Some(node) = row() {
            spawn(async move {
                if let Some(measured) = measure(&node).await {
                    reach.set(measured);
                }
            });
        }
    };

    rsx! {
        div { class: "browser-viewport",
            button {
                class: if reach().at_start { "row-arrow left at-edge" } else { "row-arrow left" },
                title: "Ver anteriores",
                // An arrow with nothing left behind it stays dark rather
                // than lighting up on hover and doing nothing.
                disabled: reach().at_start,
                onclick: move |_| {
                    if let Some(node) = row() {
                        spawn(async move { page_row(node, -1.0).await });
                    }
                },
                "‹"
            }
            button {
                class: if reach().at_end { "row-arrow right at-edge" } else { "row-arrow right" },
                title: "Ver siguientes",
                disabled: reach().at_end,
                onclick: move |_| {
                    if let Some(node) = row() {
                        spawn(async move { page_row(node, 1.0).await });
                    }
                },
                "›"
            }
            div {
                class: "browser-row",
                onmounted: move |event| {
                    let node = event.data();
                    row.set(Some(std::rc::Rc::clone(&node)));
                    spawn(async move {
                        if let Some(measured) = measure(&node).await {
                            reach.set(measured);
                        }
                    });
                },
                onscroll: move |_| remeasure(),
                // A narrower window can turn a row that fitted into one
                // that pages, and the other way round.
                onresize: move |_| remeasure(),
                for entry in items {
                    CoverCard {
                        key: "{entry.game.id}",
                        meta: capsule_meta(
                            stats.seconds(&entry.game.id),
                            stats.completion(&entry.game.id),
                        ),
                        favorite: favorites.contains(&entry.game.id),
                        pending_installs: pending_installs.clone(),
                        entry,
                        selected,
                        popup,
                    }
                }
            }
        }
    }
}

/// How far a shelf has scrolled and how far it can, which is what decides
/// whether either arrow has anywhere left to go.
#[derive(Clone, Copy, PartialEq)]
struct RowReach {
    at_start: bool,
    at_end: bool,
}

impl Default for RowReach {
    /// A row that hasn't been measured yet is at its start and, until we
    /// know otherwise, has nowhere to go — so neither arrow lights up on a
    /// shelf that turns out to fit.
    fn default() -> Self {
        RowReach {
            at_start: true,
            at_end: true,
        }
    }
}

/// A pixel of slack either way: a smooth scroll lands a hair off the exact
/// end often enough that comparing outright leaves the arrow lit.
const EDGE_SLACK: f64 = 1.0;

async fn measure(row: &Rc<MountedData>) -> Option<RowReach> {
    let Some(offset) = row.get_scroll_offset().await.ok() else {
        return None;
    };
    let Some(scroll) = row.get_scroll_size().await.ok() else {
        return None;
    };
    let Some(rect) = row.get_client_rect().await.ok() else {
        return None;
    };
    let visible = rect.width();
    // A row measured before it has been laid out reports NaN rather than an
    // error, and NaN loses *both* comparisons below — which read as "not at
    // either end" and lit up both arrows on a row that cannot move at all.
    // No numbers is not the same as numbers saying zero.
    if !(offset.x.is_finite() && scroll.width.is_finite() && visible.is_finite()) {
        return None;
    }
    Some(RowReach {
        at_start: offset.x <= EDGE_SLACK,
        at_end: offset.x + visible >= scroll.width - EDGE_SLACK,
    })
}

/// Scrolls one shelf by most of its visible width, so a click always lands
/// on a fresh set of capsules rather than nudging by some arbitrary amount.
/// The browser clamps the target, so the last page is short rather than
/// overshooting.
async fn page_row(row: Rc<MountedData>, direction: f64) {
    let Ok(offset) = row.get_scroll_offset().await else {
        return;
    };
    let Ok(rect) = row.get_client_rect().await else {
        return;
    };
    let target = (direction * rect.width()).mul_add(0.85, offset.x);
    let _ = row
        .scroll(
            dioxus::html::geometry::PixelsVector2D::new(target.max(0.0), offset.y),
            ScrollBehavior::Smooth,
        )
        .await;
}

/// The line under a capsule: time played and how much of the achievement
/// set is done. A game never launched says so rather than showing `0.0 h`,
/// and one with nothing cached simply omits the percentage — it isn't 0%
/// of anything.
fn capsule_meta(seconds: i64, completion: Option<f64>) -> String {
    let played = match seconds {
        0 => "Sin jugar".to_string(),
        _ if seconds < 3600 => format!("{} min", (seconds / 60).max(1)),
        _ => format!("{:.1} h", seconds as f64 / 3600.0),
    };
    match completion {
        Some(value) => format!("{played} · {:.0}%", value * 100.0),
        None => played,
    }
}

#[component]
fn CoverCard(
    entry: LibraryEntry,
    /// Passed in rather than looked up here: the parent already holds the
    /// whole library's stats, and re-reading sqlite once per capsule would
    /// be hundreds of queries a render.
    meta: String,
    /// Marked on the art so a shelf can be scanned for starred games
    /// without opening any of them.
    favorite: bool,
    pending_installs: std::collections::HashSet<String>,
    mut selected: Signal<Option<String>>,
    mut popup: Signal<Option<super::LibraryPopup>>,
) -> Element {
    let game = entry.game;
    let click_id = game.id.clone();
    let menu_id = game.id.clone();
    let menu_title = game.title.clone();
    let installed = is_installed(&game, &pending_installs);

    rsx! {
        div {
            class: "cover-card",
            title: "{game.title}",
            onclick: move |_| selected.set(Some(click_id.clone())),
            oncontextmenu: move |event| {
                event.prevent_default();
                let point = event.client_coordinates();
                popup.set(Some(super::LibraryPopup::GameMenu(MenuTarget {
                    product_ids: vec![menu_id.clone()],
                    title: menu_title.clone(),
                    x: point.x,
                    y: point.y,
                })));
            },

            // Art is missing often enough that the title has to be legible
            // without it, so it's drawn over the art in both cases and the
            // fallback only changes what's behind it.
            div {
                class: match game.cover_url.as_deref() {
                    Some(_) => "cover-art",
                    None => "cover-art cover-fallback",
                },
                // An <img> rather than a background, so the URL never has to
                // survive being spliced into a CSS string. Empty alt: the
                // title is right there in the overlay below it.
                if let Some(cover) = game.cover_url.as_deref() {
                    img { class: "cover-art-img", src: "{cover}", alt: "" }
                }
                div { class: "cover-scrim" }
                span { class: "cover-art-title", "{game.title}" }
            }

            div { class: "cover-pips",
                if installed {
                    span { class: "cover-pip cover-installed", title: "Instalado", "▶" }
                }
                if favorite {
                    span { class: "cover-pip cover-favorite", title: "Favorito", "★" }
                }
            }

            span { class: "cover-meta", "{meta}" }
        }
    }
}

#[cfg(test)]
mod tests;
