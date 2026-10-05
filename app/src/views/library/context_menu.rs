//! Right-click menu for a game — where Steam puts library organisation,
//! and the only place collection membership can be changed without
//! leaving the list you're organising.

use std::rc::Rc;

use decanter_gog::{GameKind, LibraryEntry};
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable as _, KeyDownEvent, Pixels, Point, SharedString, Window,
    div, prelude::*, px,
};

use super::actions::{
    bump_organisation, game_install_path, launch_game, open_in_file_manager, resolve_launch_target, start_install,
    stop_game,
};
use super::data;
use super::install::InstallRequest;
use super::shelf_popups;
use super::{LibraryModel, LibraryPopup, LibraryView};
use crate::state::{AppState, InstallStatus};
use crate::widgets::{InputEvent, TextInput, menu, tooltip};

/// What the menu acts on, and where to draw it. A list rather than one id
/// because right-clicking inside a multi-selection acts on all of it —
/// otherwise marking ten games would still mean ten trips through the menu.
#[derive(Clone)]
pub struct MenuTarget {
    pub product_ids: Vec<String>,
    pub title: String,
    pub position: Point<Pixels>,
}

type Callback = Rc<dyn Fn(&str, &mut App)>;

/// Per-popup state that only lives while that popup is open: the name
/// field ("Nueva coleccion…", the new-collection popup) and the focus that
/// catches Escape.
pub struct PopupState {
    pub focus: FocusHandle,
    pub naming: bool,
    pub name: Entity<TextInput>,
    /// Set on every render, since what Enter does depends on the popup's
    /// current target; Escape always just closes.
    pub on_submit: Option<Callback>,
    on_cancel: Option<Rc<dyn Fn(&mut App)>>,
}

impl PopupState {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let name = cx.new(|cx| TextInput::new("Nombre", cx).text_size(12.));
        cx.subscribe(&name, |this, input, event: &InputEvent, cx| match event {
            InputEvent::Submit => {
                let text = input.read(cx).text().trim().to_owned();
                if text.is_empty() {
                    return;
                }
                if let Some(on_submit) = this.on_submit.clone() {
                    on_submit(&text, cx);
                }
            }
            InputEvent::Cancel => {
                if let Some(on_cancel) = this.on_cancel.clone() {
                    on_cancel(cx);
                }
            }
            InputEvent::Changed => {}
        })
        .detach();
        Self {
            focus,
            naming: false,
            name,
            on_submit: None,
            on_cancel: None,
        }
    }
}

/// Whichever popup the model has open, drawn over the whole library page.
/// Only one ever renders: they all live in the same `popup` slot.
pub fn render_popup(
    model: &Entity<LibraryModel>,
    window: &mut Window,
    cx: &mut Context<LibraryView>,
) -> Option<AnyElement> {
    let popup = model.read(cx).popup.clone()?;
    let profile_id = AppState::get(cx)
        .session
        .read(cx)
        .active_profile
        .clone()
        .unwrap_or_default();
    let key: SharedString = match &popup {
        LibraryPopup::GameMenu(target) => format!("game-menu-{}-{:?}", target.title, target.position).into(),
        LibraryPopup::ShelfMenu(target) => format!("shelf-menu-{}", target.collection_id).into(),
        LibraryPopup::NewCollection(position) => format!("new-collection-{position:?}").into(),
    };
    let state = window.use_keyed_state(key, cx, PopupState::new);
    let cancel_model = model.clone();
    state.update(cx, |state, _| {
        state.on_submit = None;
        state.on_cancel = Some(Rc::new(move |cx| {
            cancel_model.update(cx, |model, cx| model.close_popup(cx))
        }));
    });
    let close_model = model.clone();
    let close: menu::Dismiss = Rc::new(move |_, cx| close_model.update(cx, |model, cx| model.close_popup(cx)));
    let escape = Rc::clone(&close);
    let (position, content) = match popup {
        LibraryPopup::GameMenu(target) => {
            let position = target.position;
            (position, game_menu(target, &profile_id, &state, &close, cx))
        }
        LibraryPopup::ShelfMenu(target) => {
            let position = target.position;
            (position, shelf_popups::shelf_menu(target, &profile_id, model, &close))
        }
        LibraryPopup::NewCollection(position) => (
            position,
            shelf_popups::new_collection(&profile_id, &state, &close, window, cx),
        ),
    };
    let focus = state.read(cx).focus.clone();
    let content = div()
        .track_focus(&focus)
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if event.keystroke.key == "escape" {
                escape(window, cx);
            }
        })
        .child(content);
    Some(menu::popup(position, content, window, close))
}

fn game_menu(
    target: MenuTarget,
    profile_id: &str,
    state: &Entity<PopupState>,
    close: &menu::Dismiss,
    cx: &mut Context<LibraryView>,
) -> AnyElement {
    let games = AppState::get(cx).library.read(cx).games.clone();
    // Each product id in the target carries its own account (two accounts
    // in this profile can each own a copy), so favorites/hidden go through
    // that account, not a single one for the whole menu.
    let account_for = move |product_id: &str| {
        games
            .iter()
            .find(|entry| entry.game.id == product_id)
            .map(|entry| entry.game.account_id.clone())
    };
    // "Every one of them already is" decides which way a toggle goes, so a
    // mixed selection moves to the same state rather than inverting each
    // game independently.
    let is_fav = target
        .product_ids
        .iter()
        .all(|id| account_for(id).is_some_and(|account_id| data::is_favorite(&account_id, id)));
    let is_hidden = target
        .product_ids
        .iter()
        .all(|id| account_for(id).is_some_and(|account_id| data::hidden_ids(&account_id).contains(id)));
    let single_entry = match target.product_ids.as_slice() {
        [only_id] => AppState::get(cx)
            .library
            .read(cx)
            .games
            .iter()
            .find(|entry| &entry.game.id == only_id)
            .cloned(),
        _ => None,
    };
    let collections = data::load_collections(profile_id);

    let mut panel = menu::panel().child(menu::title(target.title.clone()));
    if let Some(entry) = single_entry.filter(|entry| entry.kind == GameKind::Game) {
        panel = panel
            .children(game_action_item(&entry, close, cx))
            .children(open_folder_item(&entry, close))
            .child(properties_item(&entry, close))
            .child(menu::separator());
    }

    let fav_ids = target.product_ids.clone();
    let fav_accounts = account_for.clone();
    let fav_close = Rc::clone(close);
    panel = panel.child(
        menu::item("menu-favorite", false)
            .on_click(move |_, window, cx| {
                tracing::info!(
                    count = fav_ids.len(),
                    favorite = !is_fav,
                    "bulk favorite toggle from context menu"
                );
                for product_id in &fav_ids {
                    if let Some(account_id) = fav_accounts(product_id) {
                        data::set_favorite(&account_id, product_id, !is_fav);
                    }
                }
                bump_organisation(cx);
                fav_close(window, cx);
            })
            .child(if is_fav {
                "Quitar de favoritos"
            } else {
                "Agregar a favoritos"
            }),
    );
    let hide_ids = target.product_ids.clone();
    let hide_accounts = account_for;
    let hide_close = Rc::clone(close);
    panel = panel.child(
        menu::item("menu-hide", false)
            .on_click(move |_, window, cx| {
                tracing::info!(
                    count = hide_ids.len(),
                    hidden = !is_hidden,
                    "bulk hide toggle from context menu"
                );
                for product_id in &hide_ids {
                    if let Some(account_id) = hide_accounts(product_id) {
                        data::set_hidden(&account_id, product_id, !is_hidden);
                    }
                }
                bump_organisation(cx);
                hide_close(window, cx);
            })
            .child(if is_hidden {
                "Mostrar en la biblioteca"
            } else {
                "Ocultar de la biblioteca"
            }),
    );

    panel = panel.child(menu::separator()).child(menu::label("COLECCIONES"));
    if collections.list.is_empty() {
        panel = panel.child(menu::empty("Todavia no creaste ninguna."));
    } else {
        let items = collections.list.iter().enumerate().map(|(index, collection)| {
            let member = target
                .product_ids
                .iter()
                .all(|id| collections.contains(&collection.id, id));
            // A dynamic collection is defined by its filter; adding a game
            // by hand would be a change the next render throws away.
            let dynamic = collection.filter.is_some();
            let product_ids = target.product_ids.clone();
            let collection_id = collection.id.clone();
            menu::item(("menu-collection", index), dynamic)
                .when(dynamic, |item| {
                    item.tooltip(tooltip("Coleccion dinamica: su contenido lo decide el filtro guardado"))
                })
                .when(!dynamic, |item| {
                    item.on_click(move |_, _, cx| {
                        for product_id in &product_ids {
                            data::set_collection_member(&collection_id, product_id, !member);
                        }
                        bump_organisation(cx);
                    })
                })
                .child(menu::check(member))
                .child(collection.name.clone())
                .when(dynamic, |item| item.child(div().ml_auto().child(auto_tag("auto"))))
        });
        panel = panel.child(
            div()
                .id("menu-collections")
                .max_h(px(200.))
                .overflow_y_scroll()
                .children(items),
        );
    }

    if state.read(cx).naming {
        let product_ids = target.product_ids;
        let profile_id = profile_id.to_owned();
        let name = state.read(cx).name.clone();
        state.update(cx, |state, _| {
            state.on_submit = Some(Rc::new(move |text, cx| {
                // Created and joined in one step: a collection made from a
                // game's menu is always meant to contain that game.
                data::create_collection(&profile_id, text);
                if let Some(created) = data::load_collections(&profile_id)
                    .list
                    .into_iter()
                    .find(|collection| collection.name == text)
                {
                    for product_id in &product_ids {
                        data::set_collection_member(&created.id, product_id, true);
                    }
                }
                name.update(cx, |input, cx| input.set_text("", cx));
                bump_organisation(cx);
            }));
        });
        panel = panel.child(div().pt_1().child(state.read(cx).name.clone()));
    } else {
        let state = state.clone();
        panel = panel.child(
            menu::item("menu-new-collection", false)
                .on_click(move |_, window, cx| {
                    state.update(cx, |state, cx| {
                        state.naming = true;
                        window.focus(&state.name.focus_handle(cx), cx);
                        cx.notify();
                    });
                })
                .child("Nueva coleccion…"),
        );
    }
    panel.into_any_element()
}

/// `.auto-tag` / `.context-menu-tag`: marks a collection whose contents
/// follow a saved filter rather than a list of games.
pub fn auto_tag(text: &str) -> gpui::Div {
    div()
        .flex_none()
        .px_1()
        .rounded(px(8.))
        .border_1()
        .border_color(gpui::rgb(crate::theme::ACCENT_BLUE_DIM))
        .text_color(gpui::rgb(crate::theme::ACCENT_BLUE))
        .text_size(px(8.5))
        .font_weight(gpui::FontWeight::BOLD)
        .child(text.to_uppercase())
}

/// Jugar/Instalar/Cancelar/Detener for the one game the menu is on — the
/// same actions the detail pane's action bar offers, reachable without
/// first clicking through to it.
fn game_action_item(entry: &LibraryEntry, close: &menu::Dismiss, cx: &App) -> Option<AnyElement> {
    let account_id = entry.game.account_id.clone();
    let background = AppState::get(cx).background.read(cx);
    let status = background.installing.get(&entry.game.id).cloned();
    let running = background.running.contains(&entry.game.id);
    let is_installed = entry.game.install_state.as_ref().is_some_and(|state| state.installed);
    let close = Rc::clone(close);
    let product_id = entry.game.id.clone();

    if matches!(status, Some(InstallStatus::Queued | InstallStatus::Installing { .. })) {
        return Some(
            menu::item("menu-action", false)
                .on_click(move |_, window, cx| {
                    let account_id = account_id.clone();
                    let product_id = product_id.clone();
                    cx.spawn(async move |_| super::install::cancel(&account_id, &product_id).await)
                        .detach();
                    close(window, cx);
                })
                .child("Cancelar instalacion")
                .into_any_element(),
        );
    }

    if !is_installed {
        // Failed sits here too: whatever partial state it left behaves the
        // same as never having started — retrying is just installing again.
        let title = entry.game.title.clone();
        return Some(
            menu::item("menu-action", false)
                .on_click(move |_, window, cx| {
                    start_install(
                        InstallRequest {
                            account_id: account_id.clone(),
                            product_id: product_id.clone(),
                            parent_id: None,
                            title: title.clone(),
                            branch: None,
                            dlcs: Vec::new(),
                        },
                        cx,
                    );
                    close(window, cx);
                })
                .child("Instalar")
                .into_any_element(),
        );
    }

    let Some(installed) = resolve_launch_target(&account_id, &entry.game) else {
        return Some(
            menu::item("menu-action", false)
                .opacity(0.5)
                .cursor_default()
                .hover(|item| item)
                .child("Instalado, pero sin ejecutable detectado")
                .into_any_element(),
        );
    };

    Some(if running {
        menu::item("menu-action", false)
            .on_click(move |_, window, cx| {
                stop_game(account_id.clone(), product_id.clone(), cx);
                close(window, cx);
            })
            .child("Detener")
            .into_any_element()
    } else {
        menu::item("menu-action", false)
            .on_click(move |_, window, cx| {
                launch_game(account_id.clone(), installed.clone(), cx);
                close(window, cx);
            })
            .child("Jugar")
            .into_any_element()
    })
}

/// Opens the install directory — needs only the files, not a resolved
/// launch executable, so it also shows in the "sin ejecutable" case.
fn open_folder_item(entry: &LibraryEntry, close: &menu::Dismiss) -> Option<AnyElement> {
    let Some(path) = game_install_path(&entry.game.account_id, &entry.game.id) else {
        return None;
    };
    let close = Rc::clone(close);
    Some(
        menu::item("menu-folder", false)
            .on_click(move |_, window, cx| {
                tracing::info!(path = %path.display(), "opening install folder in file manager");
                open_in_file_manager(&path);
                close(window, cx);
            })
            .child("Abrir carpeta del juego")
            .into_any_element(),
    )
}

/// Steam's right-click → Propiedades, the same window the detail pane's
/// own button opens.
fn properties_item(entry: &LibraryEntry, close: &menu::Dismiss) -> AnyElement {
    let game = entry.game.clone();
    let close = Rc::clone(close);
    menu::item("menu-properties", false)
        .on_click(move |_, window, cx| {
            close(window, cx);
            super::open_properties(&game, cx);
        })
        .child("Propiedades")
        .into_any_element()
}
