//! The left-hand game list: a search row with a filter toggle, a Steam-
//! style filter panel, then the list grouped by a real distinction —
//! bottles-next's own `install_state`, not a mapping of ours.
//!
//! Filter semantics follow Steam's: leaving a section empty means "no
//! constraint", checks inside a section combine, and sections combine with
//! each other. GÉNERO and LOGROS are alternatives, so they're OR'd; within
//! ESTADO they're independent properties of one game (installed, unplayed,
//! favourite), so they're AND'd — checking two there narrows rather than
//! widens. Product type is a single choice, not a section.

use std::collections::{BTreeSet, HashMap, HashSet};

use achv_core::Game;
use achv_gog::LibraryEntry;
use dioxus::prelude::*;

use super::LibraryPopup;
use super::context_menu::{ContextMenu, MenuTarget};
use super::data::{self, LibraryStats};
use super::filters::{
    AchievementState, Filters, KindSelection, PlayState, SavedFilter, Sort, is_installed, pending_install_ids,
    sort_entries,
};
use super::shelf_popups::{NewCollectionPopup, ShelfMenu, ShelfMenuTarget};
use super::shelves;
use crate::gog_library::LibraryState;

/// How many entries sit behind each filter option.
///
/// Counted against the chosen product type only, never against the other
/// filters — a count that shrank as you ticked boxes elsewhere would stop
/// answering the question it's there for: is this option worth ticking at
/// all?
#[derive(Clone, PartialEq, Default)]
struct Tallies {
    states: HashMap<PlayState, usize>,
    achievements: HashMap<AchievementState, usize>,
    genres: HashMap<String, usize>,
    collections: HashMap<String, usize>,
}

fn count_options(
    entries: &[LibraryEntry],
    favorites: &HashSet<String>,
    stats: &LibraryStats,
    collections: &data::Collections,
    pending_installs: &HashSet<String>,
) -> Tallies {
    let mut tallies = Tallies::default();

    for entry in entries {
        let id = &entry.game.id;
        let played = stats.seconds(id) > 0;

        for (state, holds) in [
            (PlayState::Installed, is_installed(&entry.game, pending_installs)),
            (PlayState::Played, played),
            (PlayState::Unplayed, !played),
            (PlayState::Favorite, favorites.contains(id)),
        ] {
            if holds {
                *tallies.states.entry(state).or_default() += 1;
            }
        }

        *tallies
            .achievements
            .entry(AchievementState::of(stats.completion(id)))
            .or_default() += 1;

        for genre in &entry.game.genres {
            *tallies.genres.entry(genre.clone()).or_default() += 1;
        }

        for collection in &collections.list {
            if collections.contains(&collection.id, id) {
                *tallies.collections.entry(collection.id.clone()).or_default() += 1;
            }
        }
    }

    tallies
}

/// Every genre present in the library. Drawn from the whole library, not
/// the filtered view — otherwise picking a genre would empty the very
/// picker it was chosen from.
fn available_genres(entries: &[LibraryEntry]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|entry| entry.game.genres.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[component]
pub(super) fn Sidebar(
    games: Signal<Vec<LibraryEntry>>,
    state: Signal<LibraryState>,
    selected: Signal<Option<String>>,
    /// Shared with `ShelfBrowser`: the sidebar's own right-click menus and
    /// the cover browser's are the same popup, so only one can be open
    /// across the whole `Library` view at once.
    mut popup: Signal<Option<LibraryPopup>>,
    /// Also shared with `ShelfBrowser`: unlike the rest of what's in here,
    /// "solo instalados" narrows the whole library view, not just this
    /// list, so it's owned by `Library` instead of created locally.
    mut filters: Signal<Filters>,
    profile_id: Option<String>,
) -> Element {
    let mut search = use_signal(String::new);
    let mut kind = use_signal(KindSelection::default);
    let mut panel_open = use_signal(|| false);
    let mut collapsed = use_signal(HashSet::<String>::new);
    // Which profile's view state is loaded, rather than a plain "loaded"
    // flag: the navbar's switcher changes the profile without remounting
    // this component, and one account's arrangement must not be written
    // over the other's.
    let mut loaded_profile = use_signal(|| None::<String>);
    // Captured for the toggle handlers, which outlive the borrow of
    // `profile_id` the render itself takes.
    let collapse_profile = profile_id.clone();
    // The id being dragged. Held here rather than in the drag event's
    // dataTransfer, which the webview doesn't expose to us reliably.
    let mut dragging = use_signal(|| None::<String>);
    // Which collection shelf is being dragged to reorder it -- a separate
    // signal from `dragging` above, which is a *game* being dragged onto
    // a shelf to join it. The two drags can't be told apart by shape
    // alone (both are just an id), so they get their own state.
    let mut dragging_shelf = use_signal(|| None::<String>);
    let mut shelf_drop_target = use_signal(|| None::<String>);
    // Ctrl-click builds this up; a plain click clears it.
    let marked = use_signal(HashSet::<String>::new);
    // Shared with the detail pane and the cover browser: any of them can
    // change a star or a collection, and all three group by them.
    let background = use_context::<crate::background::Background>();
    let collections_generation = background.organisation;
    let mut renaming = use_signal(|| None::<String>);
    let mut rename_draft = use_signal(String::new);

    let _ = collections_generation();
    // A queued or downloading game should already read as installed here,
    // not just once bottles-next's own record catches up.
    let pending_installs = pending_install_ids(&background.installing.read());
    // Restored once per profile rather than on every render, so toggling a
    // shelf (or a filter) isn't immediately undone by a reload of the saved
    // state.
    if let Some(id) = profile_id.as_deref()
        && loaded_profile.peek().as_deref() != Some(id)
    {
        collapsed.set(data::collapsed_shelves(id));
        // A filter state that no longer parses leaves the defaults alone
        // rather than half-applying itself.
        if let Some(saved) = data::view_state(id, data::SIDEBAR_FILTERS)
            .as_deref()
            .and_then(SavedFilter::decode)
        {
            kind.set(saved.kind);
            filters.set(saved.filters);
        }
        panel_open.set(data::view_state(id, data::FILTER_PANEL).as_deref() == Some("1"));
        loaded_profile.set(Some(id.to_string()));
    }

    // Saved back through the same encoding a dynamic collection uses, so
    // "save this filter as a collection" and "restore what I was looking
    // at" can't disagree.
    //
    // Keyed off `loaded_profile` rather than the prop: an effect captures
    // its closure once, at mount, when the prop is still `None` — the
    // signal is the only profile that stays current. It doubles as the
    // "restore has run" guard, so the first render can't write its
    // defaults over what's on disk.
    use_effect(move || {
        let Some(id) = loaded_profile() else { return };
        let saved = SavedFilter {
            kind: kind(),
            filters: filters.read().clone(),
        };
        if let Some(encoded) = saved.encode() {
            data::save_view_state(&id, data::SIDEBAR_FILTERS, &encoded);
        }
        data::save_view_state(&id, data::FILTER_PANEL, if panel_open() { "1" } else { "0" });
    });

    let favorite_ids = profile_id.as_deref().map(data::favorite_ids).unwrap_or_default();
    let hidden_ids = profile_id.as_deref().map(data::hidden_ids).unwrap_or_default();
    let stats = profile_id.as_deref().map(data::library_stats).unwrap_or_default();
    let collections = profile_id.as_deref().map(data::load_collections).unwrap_or_default();

    let query = search.read().to_lowercase();
    let current = filters.read().clone();
    let all_games = games.read();
    let genres = available_genres(&all_games);
    let matching: Vec<LibraryEntry> = all_games
        .iter()
        .filter(|entry| kind().accepts(entry.kind))
        .filter(|entry| !hidden_ids.contains(&entry.game.id))
        .filter(|entry| current.accepts(entry, &favorite_ids, &stats, &collections, &pending_installs))
        .filter(|entry| query.is_empty() || entry.game.title.to_lowercase().contains(&query))
        .cloned()
        .collect();
    drop(all_games);

    let mut visible = matching;
    sort_entries(&mut visible, current.sort, &stats);

    // The denominator for the panel's result line: everything the chosen
    // product type covers, before any filter narrows it.
    let in_kind: Vec<LibraryEntry> = games
        .read()
        .iter()
        .filter(|entry| kind().accepts(entry.kind))
        // Same exclusions the list itself applies, or the "X de Y" line
        // counts rows nothing can ever show.
        .filter(|entry| entry.parent_id.is_none())
        .filter(|entry| super::filters::belongs_in_the_library(entry))
        .filter(|entry| !hidden_ids.contains(&entry.game.id))
        .cloned()
        .collect();
    let kind_total = in_kind.len();
    let tallies = count_options(&in_kind, &favorite_ids, &stats, &collections, &pending_installs);

    let nothing_to_show = visible.is_empty();
    let shelves = shelves::build(&visible, &collections, &favorite_ids, &stats, &pending_installs);
    let active_count = current.active_count();
    // Collections only, in display order -- what a drag-reorder drop
    // needs to turn "dropped on this shelf" into a delta for
    // `move_collection`, and what the shelf menu needs to grey out
    // Subir/Bajar at either end.
    let collections_order: Vec<String> = collections.list.iter().map(|c| c.id.clone()).collect();

    rsx! {
        div { class: "library-sidebar",
            div { class: "library-search",
                div { class: "kind-row",
                    select {
                        class: "kind-select",
                        value: "{kind().value()}",
                        onchange: move |event| kind.set(KindSelection::from_value(&event.value())),
                        option { value: "games", "Juegos" }
                        option { value: "games_extras", "Juegos y extras" }
                        option { value: "extras", "Extras" }
                        option { value: "movies", "Peliculas" }
                        option { value: "all", "Todo" }
                    }

                    // Shortcuts to two panel settings that get reached for
                    // constantly. They toggle the same state the panel
                    // edits, so the two never disagree.
                    button {
                        class: if current.sort == Sort::RecentlyPlayed { "icon-btn quick-toggle active" } else { "icon-btn quick-toggle" },
                        title: "Ordenar por actividad reciente",
                        "aria-pressed": "{current.sort == Sort::RecentlyPlayed}",
                        onclick: move |_| {
                            let mut next = filters.read().clone();
                            next.sort = if next.sort == Sort::RecentlyPlayed {
                                Sort::default()
                            } else {
                                Sort::RecentlyPlayed
                            };
                            filters.set(next);
                        },
                        "🔥"
                    }
                    button {
                        class: if current.states.contains(&PlayState::Installed) { "icon-btn quick-toggle active" } else { "icon-btn quick-toggle" },
                        title: "Mostrar solo instalados",
                        "aria-pressed": "{current.states.contains(&PlayState::Installed)}",
                        onclick: move |_| {
                            let mut next = filters.read().clone();
                            if !next.states.insert(PlayState::Installed) {
                                next.states.remove(&PlayState::Installed);
                            }
                            filters.set(next);
                        },
                        "▶"
                    }
                    button {
                        class: "icon-btn quick-toggle",
                        title: "Nueva coleccion",
                        onclick: move |event: MouseEvent| {
                            let point = event.client_coordinates();
                            popup.set(Some(LibraryPopup::NewCollection { x: point.x, y: point.y }));
                        },
                        "+"
                    }
                }

                div { class: "search-row",
                    input {
                        placeholder: "Buscar...",
                        value: "{search}",
                        oninput: move |event| search.set(event.value()),
                    }
                    if !search.read().is_empty() {
                        button {
                            class: "icon-btn search-clear",
                            title: "Limpiar busqueda",
                            onclick: move |_| search.set(String::new()),
                            "✕"
                        }
                    }
                    button {
                        class: if panel_open() || active_count > 0 { "icon-btn filter-toggle active" } else { "icon-btn filter-toggle" },
                        title: "Filtros",
                        onclick: move |_| {
                            let open = panel_open();
                            panel_open.set(!open);
                            // Opening the panel tidies away any stray
                            // right-click menu rather than sitting behind
                            // or beside it.
                            if !open {
                                popup.set(None);
                            }
                        },
                        "☰"
                        if active_count > 0 {
                            span { class: "filter-badge", "{active_count}" }
                        }
                    }
                }

            }

            div { class: "library-list",
                match &*state.read() {
                    LibraryState::Idle => rsx! {
                        p { class: "muted", style: "padding: 0 14px;", "Elegi un perfil arriba." }
                    },
                    LibraryState::Failed(err) => rsx! {
                        p { class: "error", style: "margin: 0 14px;", "{err}" }
                    },
                    // `Loading` never reaches here -- `Library` shows its
                    // own full-screen splash for that and doesn't mount
                    // this component until the state moves past it.
                    _ if nothing_to_show => rsx! {
                        p { class: "muted", style: "padding: 0 14px;", "Sin resultados." }
                    },
                    _ => rsx! {
                        for shelf in shelves {
                            {
                                let shelf_id = shelf.id.clone();
                                let shelf_title = shelf.title.clone();
                                // One clone per shelf: each row's handler owns
                                // its own, since they all outlive this loop.
                                let collapse_profile = collapse_profile.clone();
                                let drag_profile = collapse_profile.clone();
                                let drop_collection = shelf.collection_id.clone().filter(|_| !shelf.dynamic);
                                let drop_target = drop_collection.is_some();
                                let menu_shelf = shelf.collection_id.clone();
                                // Reordering by drag applies to any real
                                // collection, dynamic or not -- unlike a game
                                // drop, it never touches which games are
                                // members of anything.
                                let reorder_id = shelf.collection_id.clone();
                                let is_reorderable = reorder_id.is_some();
                                // Every drag handler below needs its own
                                // copy: each is a separate `move` closure,
                                // so one shared value can't be captured by
                                // more than one of them.
                                let dragstart_id = reorder_id.clone();
                                let dragover_id = reorder_id.clone();
                                let drop_id = reorder_id.clone();
                                let shelf_collections_order = collections_order.clone();
                                let is_dragging_this = is_reorderable && dragging_shelf.read().as_deref() == reorder_id.as_deref();
                                let is_reorder_target = is_reorderable
                                    && shelf_drop_target.read().as_deref() == reorder_id.as_deref();
                                let is_collapsed = collapsed.read().contains(&shelf_id);
                                let is_renaming = renaming.read().as_deref() == shelf.collection_id.as_deref()
                                    && shelf.collection_id.is_some();
                                let installed = shelf.installed_count(&pending_installs);
                                let total = shelf.entries.len();
                                let is_dynamic = shelf.dynamic;
                                let shelf_class = match (is_dragging_this, is_reorder_target) {
                                    (true, _) => "shelf dragging",
                                    (false, true) => "shelf reorder-target",
                                    _ => "shelf",
                                };
                                rsx! {
                                    div { class: "{shelf_class}", key: "{shelf.id}",
                                        if is_renaming {
                                            input {
                                                class: "shelf-rename",
                                                value: "{rename_draft}",
                                                autofocus: true,
                                                oninput: move |event| rename_draft.set(event.value()),
                                                onkeydown: {
                                                    let id = shelf.collection_id.clone().unwrap_or_default();
                                                    move |event: KeyboardEvent| {
                                                        match event.key() {
                                                            Key::Enter => {
                                                                let name = rename_draft.read().trim().to_string();
                                                                if !name.is_empty() {
                                                                    data::rename_collection(&id, &name);
                                                                    crate::background::bump_organisation(collections_generation);
                                                                }
                                                                renaming.set(None);
                                                            }
                                                            Key::Escape => renaming.set(None),
                                                            _ => {}
                                                        }
                                                    }
                                                },
                                            }
                                        }
                                        button {
                                            class: if drop_target { "shelf-head shelf-drop" } else { "shelf-head" },
                                            // The header doubles as the drag
                                            // handle for reordering, the way
                                            // Steam's own collection rows do --
                                            // only real collections can be
                                            // picked up this way.
                                            draggable: is_reorderable,
                                            ondragstart: move |_| {
                                                if let Some(id) = dragstart_id.clone() {
                                                    dragging_shelf.set(Some(id));
                                                }
                                            },
                                            ondragend: move |_| {
                                                dragging_shelf.set(None);
                                                shelf_drop_target.set(None);
                                            },
                                            // A shelf drag in progress takes
                                            // priority over the game-drop
                                            // target below -- the two can't
                                            // both be happening on the same
                                            // header at once, but only one of
                                            // these signals is ever set.
                                            ondragover: move |event| {
                                                if dragging_shelf.read().is_some() {
                                                    if is_reorderable && !is_dragging_this {
                                                        event.prevent_default();
                                                        shelf_drop_target.set(dragover_id.clone());
                                                    }
                                                } else if drop_target {
                                                    event.prevent_default();
                                                }
                                            },
                                            ondragleave: move |_| {
                                                if is_reorder_target {
                                                    shelf_drop_target.set(None);
                                                }
                                            },
                                            ondrop: move |event| {
                                                event.prevent_default();
                                                if let Some(dragged_id) = dragging_shelf() {
                                                    if let (Some(from), Some(to)) = (
                                                        shelf_collections_order.iter().position(|id| id == &dragged_id),
                                                        drop_id.as_ref().and_then(|target| {
                                                            shelf_collections_order.iter().position(|id| id == target)
                                                        }),
                                                    ) && let Some(profile_id) = drag_profile.as_deref()
                                                    {
                                                        let delta = to as i64 - from as i64;
                                                        if delta != 0 {
                                                            data::move_collection(profile_id, &dragged_id, delta);
                                                            crate::background::bump_organisation(collections_generation);
                                                        }
                                                    }
                                                    dragging_shelf.set(None);
                                                    shelf_drop_target.set(None);
                                                    return;
                                                }
                                                let (Some(product_id), Some(collection_id)) =
                                                    (dragging(), drop_collection.clone())
                                                else {
                                                    return;
                                                };
                                                data::set_collection_member(&collection_id, &product_id, true);
                                                dragging.set(None);
                                                crate::background::bump_organisation(collections_generation);
                                            },
                                            oncontextmenu: move |event| {
                                                event.prevent_default();
                                                let Some(id) = menu_shelf.clone() else { return };
                                                let point = event.client_coordinates();
                                                popup.set(Some(LibraryPopup::ShelfMenu(ShelfMenuTarget {
                                                    collection_id: id,
                                                    title: shelf_title.clone(),
                                                    x: point.x,
                                                    y: point.y,
                                                })));
                                            },
                                            onclick: move |_| {
                                                {
                                                    let mut open = collapsed.write();
                                                    if !open.insert(shelf_id.clone()) {
                                                        open.remove(&shelf_id);
                                                    }
                                                }
                                                if let Some(id) = collapse_profile.as_deref() {
                                                    data::save_collapsed_shelves(id, &collapsed.read());
                                                }
                                            },
                                            span { class: "shelf-chevron", if is_collapsed { "▸" } else { "▾" } }
                                            span { class: "shelf-title", "{shelf.title}" }
                                            // The browser tags these too; a collection
                                            // shouldn't look hand-made in one pane and
                                            // generated in the other.
                                            if is_dynamic {
                                                span { class: "auto-tag", "AUTO" }
                                            }
                                            // Steam's (installed/total) — how much of
                                            // this shelf is actually playable right now.
                                            span { class: "shelf-count", "({installed}/{total})" }
                                        }

                                        if !is_collapsed {
                                            if shelf.entries.is_empty() {
                                                div { class: "shelf-empty", "Vacia" }
                                            } else {
                                                for entry in shelf.entries {
                                                    SidebarItem {
                                                        key: "{entry.game.id}",
                                                        installed: is_installed(&entry.game, &pending_installs),
                                                        game: entry.game,
                                                        selected,
                                                        popup,
                                                        dragging,
                                                        marked,
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }

            // Only one of these ever renders at once: they all read the
            // same `popup`, so opening any of them already cleared it of
            // whichever the other two might have left there.
            match popup() {
                Some(LibraryPopup::GameMenu(target)) => rsx! {
                    ContextMenu {
                        target,
                        profile_id: profile_id.clone().unwrap_or_default(),
                        collections: collections.clone(),
                        generation: collections_generation,
                        games,
                        on_close: move |()| popup.set(None),
                    }
                },
                Some(LibraryPopup::ShelfMenu(target)) => {
                    let rename_id = target.collection_id.clone();
                    let rename_title = target.title.clone();
                    let position = collections_order.iter().position(|id| id == &target.collection_id);
                    let can_move_up = position.is_some_and(|index| index > 0);
                    let can_move_down = position.is_some_and(|index| index + 1 < collections_order.len());
                    rsx! {
                        ShelfMenu {
                            target,
                            profile_id: profile_id.clone().unwrap_or_default(),
                            generation: collections_generation,
                            can_move_up,
                            can_move_down,
                            on_rename: move |()| {
                                rename_draft.set(rename_title.clone());
                                renaming.set(Some(rename_id.clone()));
                            },
                            on_close: move |()| popup.set(None),
                        }
                    }
                }
                Some(LibraryPopup::NewCollection { x, y }) => rsx! {
                    NewCollectionPopup {
                        profile_id: profile_id.clone().unwrap_or_default(),
                        x,
                        y,
                        generation: collections_generation,
                        on_close: move |()| popup.set(None),
                    }
                },
                None => rsx! {},
            }
        }

        // Sibling of the sidebar, not a child: `.library-sidebar` hides its
        // overflow, so a 720px overlay nested inside a 280px column would
        // simply be clipped away.
        if panel_open() {
            FilterPanel {
                filters,
                genres,
                tallies: tallies,
                collections: collections
                    .list
                    .iter()
                    .filter(|c| c.filter.is_none())
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect::<Vec<_>>(),
                matched: visible.len(),
                total: kind_total,
                kind: kind(),
                profile_id: profile_id.unwrap_or_default(),
                generation: collections_generation,
                on_close: move |()| panel_open.set(false),
            }
        }
    }
}

#[component]
#[expect(
    clippy::redundant_clone,
    reason = "false positive attributed to this rsx! call site by clippy's MIR analysis of the \
              macro's own generated code, not a clone in this function's source"
)]
fn FilterPanel(
    filters: Signal<Filters>,
    genres: Vec<String>,
    tallies: Tallies,
    /// Manual collections only — a dynamic one is itself a filter, and
    /// offering it here would mean filtering a filter by a filter.
    collections: Vec<(String, String)>,
    /// How many entries survive the current filters, against how many the
    /// chosen product type has at all — a filtered library that comes back
    /// nearly empty should say why, not just look broken.
    matched: usize,
    total: usize,
    kind: KindSelection,
    profile_id: String,
    generation: Signal<u32>,
    on_close: EventHandler<()>,
) -> Element {
    let current = filters.read().clone();
    let mut saving = use_signal(|| false);
    let mut save_name = use_signal(String::new);
    let active = current.active_count();

    rsx! {
        // Sibling of the panel, not a wrapper around it: a full-bleed
        // click target behind it so clicking anywhere outside closes it,
        // the same as every other popup in this view.
        div {
            class: "context-backdrop",
            onclick: move |_| on_close.call(()),
        }
        section {
            class: "filters",
            tabindex: "-1",
            autofocus: true,
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    on_close.call(());
                }
            },
            header { class: "filters-head",
                h2 { "Filtros de biblioteca" }
                if !current.is_default() {
                    button {
                        class: "link-btn",
                        onclick: move |_| {
                            // Sort survives a reset: it's a view preference,
                            // not a constraint the user is clearing.
                            let sort = filters.read().sort;
                            filters.set(Filters { sort, ..Filters::default() });
                        },
                        "Limpiar filtros"
                    }
                }
                button { class: "link-btn", onclick: move |_| on_close.call(()), "✕" }
            }

            div { class: "filters-grid",
                div { class: "fgroup",
                    div { class: "fgroup-title", "Estado" }
                    p { class: "fgroup-note", "Propiedades del mismo juego: marcar dos acota." }
                    for (state, label) in [
                        (PlayState::Installed, "Instalados"),
                        (PlayState::Played, "Jugados"),
                        (PlayState::Unplayed, "Sin jugar"),
                        (PlayState::Favorite, "Favoritos"),
                    ] {
                        CheckRow {
                            key: "{label}",
                            label: label.to_string(),
                            tally: tallies.states.get(&state).copied().unwrap_or(0),
                            checked: current.states.contains(&state),
                            on_toggle: move |()| {
                                let mut next = filters.read().clone();
                                if !next.states.insert(state) {
                                    next.states.remove(&state);
                                }
                                filters.set(next);
                            },
                        }
                    }
                }

                div { class: "fgroup",
                    div { class: "fgroup-title", "Logros" }
                    p { class: "fgroup-note", "Segun lo que achv tiene cacheado." }
                    for (achievement, label) in [
                        (AchievementState::Complete, "Completados"),
                        (AchievementState::InProgress, "En progreso"),
                        (AchievementState::NotStarted, "Sin empezar"),
                        (AchievementState::Unmeasured, "Sin logros"),
                    ] {
                        CheckRow {
                            key: "{label}",
                            label: label.to_string(),
                            tally: tallies.achievements.get(&achievement).copied().unwrap_or(0),
                            checked: current.achievements.contains(&achievement),
                            on_toggle: move |()| {
                                let mut next = filters.read().clone();
                                if !next.achievements.insert(achievement) {
                                    next.achievements.remove(&achievement);
                                }
                                filters.set(next);
                            },
                        }
                    }
                }

                div { class: "fgroup",
                    div { class: "fgroup-title", "Orden de la lista" }
                    p { class: "fgroup-note", "Cada estanteria de portadas lleva el suyo." }
                    select {
                        value: "{current.sort.value()}",
                        onchange: move |event| {
                            let mut next = filters.read().clone();
                            next.sort = Sort::from_value(&event.value());
                            filters.set(next);
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

                    if !collections.is_empty() {
                        div { class: "fgroup-title", style: "margin-top: 16px", "Coleccion" }
                        select {
                            value: current.collection.clone().unwrap_or_default(),
                            onchange: move |event| {
                                let value = event.value();
                                let mut next = filters.read().clone();
                                next.collection = (!value.is_empty()).then_some(value);
                                filters.set(next);
                            },
                            option { value: "", "Cualquier coleccion" }
                            for (id, name) in collections.clone() {
                                {
                                    let count = tallies.collections.get(&id).copied().unwrap_or(0);
                                    rsx! { option { key: "{id}", value: "{id}", "{name} ({count})" } }
                                }
                            }
                        }
                    }
                }

                div { class: "fgroup fgroup-wide",
                    div { class: "fgroup-title", "Genero" }
                    p { class: "fgroup-note", "Alternativas: marcar dos amplia el resultado." }
                    if genres.is_empty() {
                        span { class: "muted", "Sin generos todavia." }
                    } else {
                        div { class: "genre-grid",
                            for name in genres {
                                CheckRow {
                                    key: "{name}",
                                    label: name.clone(),
                                    tally: tallies.genres.get(&name).copied().unwrap_or(0),
                                    checked: current.genres.contains(&name),
                                    on_toggle: move |()| {
                                        let mut next = filters.read().clone();
                                        if !next.genres.insert(name.clone()) {
                                            next.genres.remove(&name);
                                        }
                                        filters.set(next);
                                    },
                                }
                            }
                        }
                    }
                }
            }

            footer { class: "filters-foot",
                span { class: "filters-result",
                    if active == 0 {
                        "{total} productos"
                    } else {
                        "{matched} de {total} coinciden · {active} filtros"
                    }
                }

                // Naming is the only step this adds: the filter itself is
                // already whatever the panel above is set to.
                if saving() {
                    input {
                        placeholder: "Nombre de la coleccion",
                        value: "{save_name}",
                        autofocus: true,
                        oninput: move |event| save_name.set(event.value()),
                        onkeydown: {
                            let profile_id = profile_id;
                            move |event: KeyboardEvent| {
                                match event.key() {
                                    Key::Enter => {
                                        let name = save_name.read().trim().to_string();
                                        if name.is_empty() || profile_id.is_empty() {
                                            return;
                                        }
                                        // Stores the filter itself, not the
                                        // games it matches right now — that
                                        // is what makes it dynamic.
                                        let saved = SavedFilter { kind, filters: filters.read().clone() };
                                        if let Some(encoded) = saved.encode() {
                                            data::create_dynamic_collection(&profile_id, &name, &encoded);
                                            crate::background::bump_organisation(generation);
                                        }
                                        save_name.set(String::new());
                                        saving.set(false);
                                    }
                                    Key::Escape => saving.set(false),
                                    _ => {}
                                }
                            }
                        },
                    }
                } else {
                    button {
                        class: "save-btn",
                        onclick: move |_| saving.set(true),
                        "Guardar como coleccion dinamica"
                    }
                }
            }
        }
    }
}

#[component]
fn CheckRow(
    label: String,
    /// How many entries this option would match. Shown so an empty result
    /// is predictable before the click, not discovered after it.
    tally: usize,
    checked: bool,
    on_toggle: EventHandler<()>,
) -> Element {
    rsx! {
        label { class: "check",
            input {
                r#type: "checkbox",
                checked,
                onchange: move |_| on_toggle.call(()),
            }
            span { class: "check-label", "{label}" }
            span { class: "tally", "{tally}" }
        }
    }
}

#[component]
fn SidebarItem(
    game: Game,
    installed: bool,
    mut selected: Signal<Option<String>>,
    mut popup: Signal<Option<LibraryPopup>>,
    mut dragging: Signal<Option<String>>,
    mut marked: Signal<HashSet<String>>,
) -> Element {
    let game_id = game.id.clone();
    let drag_id = game.id.clone();
    let mark_id = game.id.clone();
    let is_marked = marked.read().contains(&game.id);
    let menu_id = game.id.clone();
    let menu_title = game.title.clone();
    let is_active = selected.read().as_deref() == Some(game_id.as_str());

    rsx! {
        div {
            class: match (is_active, is_marked) {
                (true, _) => "library-sidebar-item active",
                (false, true) => "library-sidebar-item marked",
                _ => "library-sidebar-item",
            },
            key: "{game.id}",
            draggable: true,
            ondragstart: move |_| dragging.set(Some(drag_id.clone())),
            ondragend: move |_| dragging.set(None),
            onclick: move |event: MouseEvent| {
                // Ctrl-click extends a selection to act on in bulk; a plain
                // click is still just "show me this one".
                if event.modifiers().ctrl() {
                    let mut set = marked.write();
                    if !set.insert(mark_id.clone()) {
                        set.remove(&mark_id);
                    }
                    return;
                }
                marked.write().clear();
                selected.set(Some(game_id.clone()));
            },
            oncontextmenu: move |event| {
                // Without this the webview shows its own menu instead.
                event.prevent_default();
                let point = event.client_coordinates();
                // Right-clicking a marked game targets the whole marked
                // set; right-clicking anything else targets just it.
                let selection = marked.read();
                let (product_ids, title) = if selection.contains(&menu_id) && selection.len() > 1 {
                    (
                        selection.iter().cloned().collect::<Vec<_>>(),
                        format!("{} juegos", selection.len()),
                    )
                } else {
                    (vec![menu_id.clone()], menu_title.clone())
                };
                drop(selection);
                popup.set(Some(LibraryPopup::GameMenu(MenuTarget {
                    product_ids,
                    title,
                    x: point.x,
                    y: point.y,
                })));
            },
            if let Some(icon) = &game.icon_url {
                img { src: "{icon}", alt: "" }
            } else {
                div { class: "library-sidebar-item-fallback" }
            }
            span { class: "sidebar-item-title", "{game.title}" }
            if installed {
                span { class: "sidebar-item-dot", title: "Instalado" }
            }
        }
    }
}
