//! The single "games" section: a Steam-like sidebar of the profile's GOG
//! library on the left, and a detail pane on the right. Bottles stays the
//! only other top-level section. Split into three pieces: `data` (sqlite
//! achievement reads), `sidebar` (list + search), `detail` (hero, play
//! action, achievements) — this file only wires them together.

use dioxus::prelude::*;

mod browser;
mod context_menu;
mod data;
mod detail;
mod filters;
mod fmt;
pub(crate) mod install;
mod properties;
mod shelf_popups;
mod shelves;
mod sidebar;

use context_menu::MenuTarget;
use data::PollStatus;
use detail::DetailPane;
use shelf_popups::ShelfMenuTarget;
use sidebar::Sidebar;

use crate::background::Background;
use crate::gog_library::{GogLibrary, LibraryState};
use crate::{ActiveProfile, CoreResource, LibraryHome};

/// The one context-menu-family popup this whole view can have open at a
/// time -- a game's context menu, a shelf's, or the "new collection"
/// popup. Shared between `Sidebar` and `ShelfBrowser` (the sidebar's own
/// game rows and the cover browser's capsules both open a `GameMenu`) so
/// that opening any of these anywhere in `Library` closes whichever other
/// one was already open, instead of the two stacking on screen -- the way
/// a right-click anywhere in a real app replaces whatever menu was up.
///
/// The filter panel is deliberately not one of these: it's a persistent
/// drawer with its own explicit close, not a transient menu, and closing
/// it just because an unrelated right-click happened elsewhere would lose
/// context the user didn't ask to lose.
#[derive(Clone, PartialEq)]
enum LibraryPopup {
    GameMenu(MenuTarget),
    ShelfMenu(ShelfMenuTarget),
    NewCollection { x: f64, y: f64 },
}

#[component]
pub fn Library() -> Element {
    let core = use_context::<CoreResource>();
    let active_profile = use_context::<ActiveProfile>();
    let GogLibrary { games, state, .. } = use_context::<GogLibrary>();
    // `running`/`installing` live at the app root (see `background`), not
    // here — both get written from `spawn_forever`'d tasks, which always
    // run in the root scope, so anything they touch has to be owned there
    // too or the write dangles the moment this component unmounts.
    let Background {
        running, installing, ..
    } = use_context::<Background>();
    let mut selected = use_signal(|| None::<String>);
    let mut popup = use_signal(|| None::<LibraryPopup>);
    let library_home = use_context::<LibraryHome>();

    // Clicking "Biblioteca" in the navbar always drops back to the shelf
    // browser, even when it's already the active route -- a route change
    // to the same route wouldn't rerun anything on its own, since `Library`
    // never unmounts, so whatever detail pane was open would otherwise
    // just stay put.
    use_effect(move || {
        let _ = library_home();
        selected.set(None);
        popup.set(None);
    });
    // Owned here, not by `Sidebar`, so `ShelfBrowser` can see the same
    // "solo instalados" state: that one condition narrows the whole
    // library view, not just the sidebar list, unlike the rest of what's
    // in here (search, genre, achievement) which stays sidebar-only.
    let filters = use_signal(filters::Filters::default);
    let mut poll_generation = use_signal(|| 0_u32);
    let mut poll_status = use_signal(|| PollStatus::Idle);

    // Selecting a game used to show only whatever `achv-tray
    // achievements` had last cached, by hand. Poll GOG live right here
    // instead — cache still shows instantly, this just refreshes it. The
    // status (not just the resulting cache) is tracked so the empty case
    // can say *why* it's empty instead of always pointing at a CLI command.
    use_effect(move || {
        let profile_id = active_profile.read().clone();
        let product_id = selected.read().clone();
        match (profile_id, product_id) {
            (Some(profile_id), Some(product_id)) => {
                poll_status.set(PollStatus::Polling);
                spawn(async move {
                    match data::poll_achievements(profile_id, product_id).await {
                        Ok(()) => poll_status.set(PollStatus::Done),
                        Err(err) => poll_status.set(PollStatus::Failed(err)),
                    }
                    poll_generation.set(poll_generation() + 1);
                });
            }
            _ => poll_status.set(PollStatus::Idle),
        }
    });

    // Read all three synchronously (before any await) so this reruns
    // whenever any changes, without needing `use_reactive!` for a plain
    // sync sqlite lookup.
    let achievements = use_resource(move || {
        let _ = poll_generation();
        let profile_id = active_profile.read().clone();
        let product_id = selected.read().clone();
        async move {
            match (profile_id, product_id) {
                (Some(profile_id), Some(product_id)) => Some(data::load_achievements(profile_id, product_id)),
                _ => None,
            }
        }
    });

    let selected_game = selected.read().as_ref().and_then(|id| {
        games
            .read()
            .iter()
            .find(|entry| &entry.game.id == id)
            .map(|entry| entry.game.clone())
    });
    let profile_id = active_profile.read().clone();

    // The whole view hides behind a Steam-style loading screen until this
    // session's very first sync is fully done -- see `gog_library::stream`.
    // Cheaper than letting the sidebar and detail pane mount on a
    // half-resolved library and watch fields change under them as the
    // rest of the fetch lands.
    if matches!(state(), LibraryState::Loading) {
        return rsx! { LibraryLoading { count: games.read().len() } };
    }

    rsx! {
        div { class: "library-page",
            Sidebar { games, state, selected, popup, filters, profile_id: profile_id.clone() }
            div { class: "library-detail",
                match (selected_game, profile_id) {
                    // `library-detail-inner` carries the pane's bottom
                    // padding. `DetailPane` returns a fragment, so without a
                    // wrapper here the rule matched nothing and the last
                    // achievement row sat flush against the bottom edge.
                    (Some(game), Some(profile_id)) => rsx! {
                        div { class: "library-detail-inner",
                            DetailPane { game, profile_id, achievements, poll_status: poll_status(), core, running, installing }
                        }
                    },
                    (Some(_), None) => rsx! {
                        div { class: "empty", style: "margin: 32px;", "Elegi un perfil arriba." }
                    },
                    // Nothing selected: the shelf browser stands in for an
                    // empty pane, so the default view is the library rather
                    // than a prompt to go find it.
                    (None, Some(profile_id)) => rsx! {
                        browser::ShelfBrowser { games, profile_id, selected, popup, filters }
                    },
                    _ => rsx! {
                        div { class: "empty", style: "margin: 32px;", "Elegi un perfil arriba." }
                    },
                }
            }
        }
    }
}

/// The splash a first sync sits behind, the way Steam's own "Syncing..."
/// screen does — full-width rather than tucked into the sidebar list, so
/// there's nothing half-drawn (a sidebar with no detail pane, a detail
/// pane for a game whose art hasn't landed) to look at while it waits.
#[component]
fn LibraryLoading(count: usize) -> Element {
    rsx! {
        div { class: "library-loading",
            div { class: "library-loading-inner",
                h2 { "Cargando tu biblioteca" }
                div { class: "bar-track indeterminate", div { class: "bar-fill" } }
                p { class: "muted",
                    if count == 0 {
                        "Buscando tus juegos en GOG..."
                    } else {
                        "{count} juego(s) encontrados, completando datos..."
                    }
                }
            }
        }
    }
}
