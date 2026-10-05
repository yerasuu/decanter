//! Steam puts every shelf-level operation behind a right-click, and
//! collection creation behind its own small popup rather than an inline
//! form sitting in the list — nothing lives on the row itself but the
//! name and count. This is the sidebar's equivalent of `context_menu`,
//! scoped to a shelf (or to no shelf at all, for "new collection").

use std::rc::Rc;

use gpui::{AnyElement, App, Context, Entity, Focusable as _, Pixels, Point, Window, div, prelude::*};

use super::actions::bump_organisation;
use super::context_menu::PopupState;
use super::data;
use super::{LibraryModel, LibraryView};
use crate::widgets::menu;

#[derive(Clone)]
pub struct ShelfMenuTarget {
    pub collection_id: String,
    pub title: String,
    pub position: Point<Pixels>,
}

/// Rename, reorder and delete for one collection shelf. Reordering here is
/// the click-reachable fallback; dragging the shelf header is the fast path.
pub fn shelf_menu(
    target: ShelfMenuTarget,
    profile_id: &str,
    model: &Entity<LibraryModel>,
    close: &menu::Dismiss,
) -> AnyElement {
    let order: Vec<String> = data::load_collections(profile_id)
        .list
        .into_iter()
        .map(|collection| collection.id)
        .collect();
    let index = order.iter().position(|id| id == &target.collection_id);
    let can_move_up = index.is_some_and(|index| index > 0);
    let can_move_down = index.is_some_and(|index| index + 1 < order.len());

    let move_item = |id: &'static str, label: &'static str, delta: i64, enabled: bool| {
        let profile_id = profile_id.to_owned();
        let collection_id = target.collection_id.clone();
        let close = Rc::clone(close);
        menu::item(id, !enabled)
            .when(enabled, |item| {
                item.on_click(move |_, window, cx| {
                    data::move_collection(&profile_id, &collection_id, delta);
                    bump_organisation(cx);
                    close(window, cx);
                })
            })
            .child(label)
    };

    let rename_model = model.clone();
    let rename_target = (target.collection_id.clone(), target.title.clone());
    let rename_close = Rc::clone(close);
    let delete_id = target.collection_id.clone();
    let delete_close = Rc::clone(close);
    menu::panel()
        .child(menu::title(target.title.clone()))
        .child(
            menu::item("shelf-rename", false)
                .on_click(move |_, window, cx| {
                    rename_close(window, cx);
                    let rename_target = rename_target.clone();
                    rename_model.update(cx, |model, cx| {
                        model.renaming = Some(rename_target);
                        cx.notify();
                    });
                })
                .child("Renombrar"),
        )
        .child(move_item("shelf-up", "Subir", -1, can_move_up))
        .child(move_item("shelf-down", "Bajar", 1, can_move_down))
        .child(menu::separator())
        .child(
            menu::danger_item("shelf-delete")
                .on_click(move |_, window, cx| {
                    data::delete_collection(&delete_id);
                    bump_organisation(cx);
                    delete_close(window, cx);
                })
                .child("Eliminar coleccion"),
        )
        .into_any_element()
}

/// An empty collection by name — the other way to make one, next to "save
/// this filter" (the filter panel) and "make one from this game" (the game
/// menu). Opened from its own "+", since neither of those implies you're
/// starting from nothing.
pub fn new_collection(
    profile_id: &str,
    state: &Entity<PopupState>,
    close: &menu::Dismiss,
    window: &mut Window,
    cx: &mut Context<LibraryView>,
) -> AnyElement {
    let create = {
        let profile_id = profile_id.to_owned();
        let close = Rc::clone(close);
        let window_handle = window.window_handle();
        Rc::new(move |name: &str, cx: &mut App| {
            data::create_collection(&profile_id, name);
            bump_organisation(cx);
            let close = Rc::clone(&close);
            let _ = window_handle.update(cx, |_, window, cx| close(window, cx));
        })
    };
    let name = state.read(cx).name.clone();
    if !state.read(cx).naming {
        // The field is the whole popup, so it takes focus straight away.
        window.focus(&name.focus_handle(cx), cx);
        state.update(cx, |state, _| state.naming = true);
    }
    let submit = Rc::clone(&create);
    state.update(cx, |state, _| state.on_submit = Some(submit));
    let click_name = name.clone();
    menu::panel()
        .child(menu::title("Nueva coleccion"))
        .child(div().pb_1().child(name))
        .child(
            menu::item("new-collection-create", false)
                .on_click(move |_, _, cx| {
                    let text = click_name.read(cx).text().trim().to_owned();
                    if !text.is_empty() {
                        create(&text, cx);
                    }
                })
                .child("Crear"),
        )
        .into_any_element()
}
