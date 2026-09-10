//! Right-click menu for a game in the sidebar — where Steam puts library
//! organisation, and the only place collection membership can be changed
//! without leaving the list you're organising.

use achv_gog::LibraryEntry;
use dioxus::html::geometry::PixelsSize;
use dioxus::prelude::*;

use super::data::{self, Collections};
use super::detail::{game_install_path, launch_game, open_in_file_manager, resolve_launch_target, start_install};
use super::{install, properties};
use crate::background::{Background, InstallStatus};

/// Gap kept between the menu and the window edge when it has to be pulled
/// back inside.
const MENU_MARGIN: f64 = 6.0;

/// What the menu acts on, and where to draw it. A list rather than one
/// id because right-clicking inside a multi-selection acts on all of it —
/// otherwise marking ten games would still mean ten trips through the
/// menu.
#[derive(Clone, PartialEq)]
pub(super) struct MenuTarget {
    pub product_ids: Vec<String>,
    pub title: String,
    pub x: f64,
    pub y: f64,
}

#[component]
pub(super) fn ContextMenu(
    target: MenuTarget,
    profile_id: String,
    collections: Collections,
    /// Bumped whenever something here writes, so the sidebar re-reads.
    generation: Signal<u32>,
    /// So Jugar/Instalar/Cancelar can look up the one game's own state —
    /// `MenuTarget` only carries ids. `None` for a multi-selection: those
    /// three don't have a sensible meaning across more than one game at
    /// once (which bottle would "install 3 games" even use?).
    games: Signal<Vec<LibraryEntry>>,
    on_close: EventHandler<()>,
) -> Element {
    let mut naming = use_signal(|| false);
    let mut new_name = use_signal(String::new);
    // The menu opens at the pointer, which near the bottom or right edge
    // put most of it off-screen. Both measurements come from the DOM: the
    // backdrop is `inset: 0`, so its rect *is* the viewport, and the menu's
    // own rect gives the size to keep inside it. Until they arrive the raw
    // coordinates stand, so a menu opened near an edge settles into place
    // on the frame after it appears.
    let mut viewport = use_signal(|| None::<PixelsSize>);
    let mut menu_size = use_signal(|| None::<PixelsSize>);

    let (x, y) = match (viewport(), menu_size()) {
        (Some(view), Some(menu)) => (
            target.x.min(view.width - menu.width - MENU_MARGIN).max(MENU_MARGIN),
            target.y.min(view.height - menu.height - MENU_MARGIN).max(MENU_MARGIN),
        ),
        _ => (target.x, target.y),
    };

    // "Every one of them already is" decides which way a toggle goes, so
    // a mixed selection moves to the same state rather than inverting each
    // game independently.
    let all_favorite = target.product_ids.iter().all(|id| data::is_favorite(&profile_id, id));
    let hidden = data::hidden_ids(&profile_id);
    let all_hidden = target.product_ids.iter().all(|id| hidden.contains(id));
    let is_fav = all_favorite;
    let is_hidden = all_hidden;

    let bump = crate::background::bump_organisation;

    let single_entry = match target.product_ids.as_slice() {
        [only_id] => games.read().iter().find(|entry| &entry.game.id == only_id).cloned(),
        _ => None,
    };

    rsx! {
        // Catches the click that dismisses the menu, and blocks
        // interaction with the list underneath while it's open.
        div {
            class: "context-backdrop",
            onclick: move |_| on_close.call(()),
            onmounted: move |event| {
                let node = event.data();
                spawn(async move {
                    if let Ok(rect) = node.get_client_rect().await {
                        viewport.set(Some(rect.size));
                    }
                });
            },
        }

        div {
            class: "context-menu",
            style: "left: {x}px; top: {y}px;",
            // Focusable so it can catch Escape without an input to put
            // focus on -- matches the other popups' click-outside, gives
            // this one the keyboard equivalent.
            tabindex: "-1",
            autofocus: true,
            onkeydown: move |event: KeyboardEvent| {
                if event.key() == Key::Escape {
                    on_close.call(());
                }
            },
            onmounted: move |event| {
                let node = event.data();
                spawn(async move {
                    if let Ok(rect) = node.get_client_rect().await {
                        menu_size.set(Some(rect.size));
                    }
                });
            },
            // Right-clicking inside the menu shouldn't open another one.
            oncontextmenu: move |event| event.prevent_default(),

            div { class: "context-menu-title", "{target.title}" }

            if let Some(entry) = single_entry {
                GameActionItem { entry: entry.clone(), profile_id: profile_id.clone(), on_close }
                OpenFolderItem { entry: entry.clone(), profile_id: profile_id.clone(), on_close }
                PropertiesItem { entry, profile_id: profile_id.clone(), on_close }
                div { class: "context-menu-separator" }
            }

            button {
                class: "context-menu-item",
                onclick: {
                    let profile_id = profile_id.clone();
                    let product_ids = target.product_ids.clone();
                    move |_| {
                        for product_id in &product_ids {
                            data::set_favorite(&profile_id, product_id, !is_fav);
                        }
                        bump(generation);
                        on_close.call(());
                    }
                },
                if is_fav { "Quitar de favoritos" } else { "Agregar a favoritos" }
            }

            button {
                class: "context-menu-item",
                onclick: {
                    let profile_id = profile_id;
                    let product_ids = target.product_ids;
                    move |_| {
                        for product_id in &product_ids {
                            data::set_hidden(&profile_id, product_id, !is_hidden);
                        }
                        bump(generation);
                        on_close.call(());
                    }
                },
                if is_hidden { "Mostrar en la biblioteca" } else { "Ocultar de la biblioteca" }
            }

            div { class: "context-menu-separator" }
            div { class: "context-menu-label", "COLECCIONES" }

            if collections.list.is_empty() {
                div { class: "context-menu-empty", "Todavia no creaste ninguna." }
            } else {
                div { class: "context-menu-scroll",
                    for collection in collections.list.clone() {
                        {
                            let member = target
                                .product_ids
                                .iter()
                                .all(|id| collections.contains(&collection.id, id));
                            let product_ids = target.product_ids.clone();
                            let collection_id = collection.id.clone();
                            let dynamic = collection.filter.is_some();
                            rsx! {
                                button {
                                    key: "{collection.id}",
                                    class: "context-menu-item",
                                    // A dynamic collection is defined by its
                                    // filter; adding a game by hand would be a
                                    // change the next render throws away.
                                    disabled: dynamic,
                                    title: if dynamic { "Coleccion dinamica: su contenido lo decide el filtro guardado" } else { "" },
                                    onclick: move |_| {
                                        for product_id in &product_ids {
                                            data::set_collection_member(&collection_id, product_id, !member);
                                        }
                                        bump(generation);
                                    },
                                    span { class: "context-menu-check", if member { "✓" } else { "" } }
                                    span { "{collection.name}" }
                                    if dynamic {
                                        span { class: "context-menu-tag", "auto" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if naming() {
                div { class: "context-menu-form",
                    input {
                        placeholder: "Nombre",
                        value: "{new_name}",
                        autofocus: true,
                        oninput: move |event| new_name.set(event.value()),
                        onkeydown: {
                            let profile_id = profile_id.clone();
                            let product_ids = target.product_ids.clone();
                            move |event: KeyboardEvent| {
                                if event.key() != Key::Enter {
                                    return;
                                }
                                let name = new_name.read().trim().to_string();
                                if name.is_empty() {
                                    return;
                                }
                                // Created and joined in one step: a
                                // collection made from a game's menu is
                                // always meant to contain that game.
                                data::create_collection(&profile_id, &name);
                                if let Some(created) = data::load_collections(&profile_id)
                                    .list
                                    .into_iter()
                                    .find(|collection| collection.name == name)
                                {
                                    for product_id in &product_ids {
                                        data::set_collection_member(&created.id, product_id, true);
                                    }
                                }
                                new_name.set(String::new());
                                naming.set(false);
                                bump(generation);
                            }
                        },
                    }
                }
            } else {
                button {
                    class: "context-menu-item",
                    onclick: move |_| naming.set(true),
                    "Nueva coleccion…"
                }
            }
        }
    }
}

/// Jugar/Instalar/Cancelar/Detener for the one game this menu is on top
/// of -- the same actions the detail pane's action bar offers, reachable
/// without first clicking through to it. Deliberately simpler than that
/// bar in one way: installing here always targets the default bottle
/// (Bottles' own setting) rather than opening a picker, since a dropdown
/// inside a context menu is its own kind of awkward. Picking a different
/// bottle is still what the detail pane is for.
#[component]
fn GameActionItem(entry: LibraryEntry, profile_id: String, on_close: EventHandler<()>) -> Element {
    // DLC, soundtracks, movies -- nothing here has an install of its own.
    if entry.kind != achv_gog::GameKind::Game {
        return rsx! {};
    }

    let Background {
        running,
        installing,
        playtime_generation,
        ..
    } = use_context::<Background>();
    let library = use_context::<crate::gog_library::GogLibrary>();

    let status = installing.read().get(&entry.game.id).cloned();
    let is_installed = entry.game.install_state.as_ref().is_some_and(|state| state.installed);

    if matches!(status, Some(InstallStatus::Queued | InstallStatus::Installing { .. })) {
        let product_id = entry.game.id;
        return rsx! {
            button {
                class: "context-menu-item",
                onclick: move |_| {
                    spawn(install::cancel(profile_id.clone(), product_id.clone()));
                    on_close.call(());
                },
                "Cancelar instalacion"
            }
        };
    }

    if !is_installed {
        // Failed sits here too: whatever partial state it left behaves
        // the same as never having started -- retrying is just installing
        // again, and this menu isn't the place for the detail pane's
        // "descartar" step.
        let product_id = entry.game.id.clone();
        let title = entry.game.title;
        return rsx! {
            button {
                class: "context-menu-item",
                onclick: move |_| {
                    start_install(
                    profile_id.clone(),
                    product_id.clone(),
                    None,
                    title.clone(),
                    None,
                    Vec::new(),
                    installing,
                    library,
                );
                    on_close.call(());
                },
                "Instalar"
            }
        };
    }

    let Some(installed) = resolve_launch_target(&profile_id, &entry.game) else {
        return rsx! {
            span {
                class: "context-menu-item",
                style: "opacity: .5; cursor: default;",
                "Instalado, pero sin ejecutable detectado"
            }
        };
    };

    let product_id = entry.game.id;
    if running.read().contains(&product_id) {
        let stop_profile = profile_id;
        rsx! {
            button {
                class: "context-menu-item",
                onclick: move |_| {
                    spawn(crate::launch::stop(stop_profile.clone(), product_id.clone()));
                    on_close.call(());
                },
                "Detener"
            }
        }
    } else {
        rsx! {
            button {
                class: "context-menu-item",
                onclick: move |_| {
                    dioxus::core::spawn_forever(launch_game(
                        profile_id.clone(),
                        installed.clone(),
                        running,
                        playtime_generation,
                    ));
                    on_close.call(());
                },
                "Jugar"
            }
        }
    }
}

/// Opens the game's install directory in the desktop's file manager --
/// needs only a bottle, not a resolved launch executable, so it still
/// shows up in the "instalado, pero sin ejecutable detectado" case above.
#[component]
fn OpenFolderItem(entry: LibraryEntry, profile_id: String, on_close: EventHandler<()>) -> Element {
    if entry.kind != achv_gog::GameKind::Game {
        return rsx! {};
    }

    let Some(path) = game_install_path(&profile_id, &entry.game.id) else {
        return rsx! {};
    };

    rsx! {
        button {
            class: "context-menu-item",
            onclick: move |_| {
                open_in_file_manager(&path);
                on_close.call(());
            },
            "Abrir carpeta del juego"
        }
    }
}

/// Steam's right-click → Propiedades, opening the same window the detail
/// pane's own Propiedades button does -- reachable without first clicking
/// through to the game.
#[component]
fn PropertiesItem(entry: LibraryEntry, profile_id: String, on_close: EventHandler<()>) -> Element {
    if entry.kind != achv_gog::GameKind::Game {
        return rsx! {};
    }

    rsx! {
        button {
            class: "context-menu-item",
            onclick: move |_| {
                properties::open_properties_window(entry.game.clone(), profile_id.clone());
                on_close.call(());
            },
            "Propiedades"
        }
    }
}
