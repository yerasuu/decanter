//! Steam puts every shelf-level operation behind a right-click, and
//! collection creation behind its own small popup rather than an inline
//! form sitting in the list — nothing lives on the row itself but the
//! name and count. This is the sidebar's equivalent of `context_menu.rs`,
//! just scoped to a shelf (or to no shelf at all, for "new collection")
//! instead of to a game.

use dioxus::html::geometry::PixelsSize;
use dioxus::prelude::*;

use super::data;

/// Gap kept between a popup and the window edge when it has to be pulled
/// back inside. Matches `context_menu.rs`'s own constant; kept as a
/// separate copy rather than a shared one since the edge-clamp math below
/// is small enough that duplicating it beats introducing a shared hook
/// for two call sites.
const MENU_MARGIN: f64 = 6.0;

#[derive(Clone, PartialEq)]
pub(super) struct ShelfMenuTarget {
    pub collection_id: String,
    pub title: String,
    pub x: f64,
    pub y: f64,
}

/// Rename, reorder and delete for one collection shelf. Reordering here is
/// the keyboard/click-reachable fallback; dragging the shelf header is the
/// fast path, same relationship browser.rs's shelves already have between
/// their own drag handle and nothing else -- this menu is what the
/// sidebar has instead, since a shelf here can't be dragged out of a list
/// of games as easily as a whole browser row can.
#[component]
pub(super) fn ShelfMenu(
    target: ShelfMenuTarget,
    profile_id: String,
    generation: Signal<u32>,
    can_move_up: bool,
    can_move_down: bool,
    on_rename: EventHandler<()>,
    on_close: EventHandler<()>,
) -> Element {
    let mut viewport = use_signal(|| None::<PixelsSize>);
    let mut menu_size = use_signal(|| None::<PixelsSize>);

    let (x, y) = match (viewport(), menu_size()) {
        (Some(view), Some(menu)) => (
            target.x.min(view.width - menu.width - MENU_MARGIN).max(MENU_MARGIN),
            target.y.min(view.height - menu.height - MENU_MARGIN).max(MENU_MARGIN),
        ),
        _ => (target.x, target.y),
    };

    let bump = crate::background::bump_organisation;

    rsx! {
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
            oncontextmenu: move |event| event.prevent_default(),

            div { class: "context-menu-title", "{target.title}" }

            button {
                class: "context-menu-item",
                onclick: move |_| {
                    on_rename.call(());
                    on_close.call(());
                },
                "Renombrar"
            }
            button {
                class: "context-menu-item",
                disabled: !can_move_up,
                onclick: {
                    let profile_id = profile_id.clone();
                    let collection_id = target.collection_id.clone();
                    move |_| {
                        data::move_collection(&profile_id, &collection_id, -1);
                        bump(generation);
                        on_close.call(());
                    }
                },
                "Subir"
            }
            button {
                class: "context-menu-item",
                disabled: !can_move_down,
                onclick: {
                    let profile_id = profile_id;
                    let collection_id = target.collection_id.clone();
                    move |_| {
                        data::move_collection(&profile_id, &collection_id, 1);
                        bump(generation);
                        on_close.call(());
                    }
                },
                "Bajar"
            }

            div { class: "context-menu-separator" }

            button {
                class: "context-menu-item context-menu-danger",
                onclick: {
                    let collection_id = target.collection_id;
                    move |_| {
                        data::delete_collection(&collection_id);
                        bump(generation);
                        on_close.call(());
                    }
                },
                "Eliminar coleccion"
            }
        }
    }
}

/// An empty collection by name -- the other way to make one, next to
/// "save this filter" (`FilterPanel`) and "make one from this game"
/// (`ContextMenu`). Opened from its own "+", not tucked inside either of
/// those, since neither implies you're starting from nothing.
#[component]
pub(super) fn NewCollectionPopup(
    profile_id: String,
    x: f64,
    y: f64,
    generation: Signal<u32>,
    on_close: EventHandler<()>,
) -> Element {
    let mut name = use_signal(String::new);
    let mut viewport = use_signal(|| None::<PixelsSize>);
    let mut menu_size = use_signal(|| None::<PixelsSize>);

    let (left, top) = match (viewport(), menu_size()) {
        (Some(view), Some(menu)) => (
            x.min(view.width - menu.width - MENU_MARGIN).max(MENU_MARGIN),
            y.min(view.height - menu.height - MENU_MARGIN).max(MENU_MARGIN),
        ),
        _ => (x, y),
    };

    rsx! {
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
            style: "left: {left}px; top: {top}px;",
            onmounted: move |event| {
                let node = event.data();
                spawn(async move {
                    if let Ok(rect) = node.get_client_rect().await {
                        menu_size.set(Some(rect.size));
                    }
                });
            },
            oncontextmenu: move |event| event.prevent_default(),

            div { class: "context-menu-title", "Nueva coleccion" }
            div { class: "context-menu-form",
                input {
                    placeholder: "Nombre",
                    value: "{name}",
                    autofocus: true,
                    oninput: move |event| name.set(event.value()),
                    onkeydown: {
                        let profile_id = profile_id.clone();
                        move |event: KeyboardEvent| {
                            match event.key() {
                                Key::Enter => {
                                    let trimmed = name.read().trim().to_string();
                                    if trimmed.is_empty() {
                                        return;
                                    }
                                    data::create_collection(&profile_id, &trimmed);
                                    crate::background::bump_organisation(generation);
                                    on_close.call(());
                                }
                                Key::Escape => on_close.call(()),
                                _ => {}
                            }
                        }
                    },
                }
            }
            button {
                class: "context-menu-item",
                onclick: move |_| {
                    let trimmed = name.read().trim().to_string();
                    if trimmed.is_empty() {
                        return;
                    }
                    data::create_collection(&profile_id, &trimmed);
                    crate::background::bump_organisation(generation);
                    on_close.call(());
                },
                "Crear"
            }
        }
    }
}
