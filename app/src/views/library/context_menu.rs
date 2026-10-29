//! Right-click menu for a game — where Steam puts library organisation,
//! and the only place collection membership can be changed without
//! leaving the list you're organising.

use std::rc::Rc;

use decanter_core::Storefront;
use decanter_core::capabilities::{Feature, Support};
use decanter_gog::{GameKind, LibraryEntry};
use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable as _, KeyDownEvent, Pixels, Point, SharedString,
    Stateful, Window, div, prelude::*, px,
};
use rust_i18n::t;

use super::actions::{
    bump_organisation, game_install_path, launch_game, open_in_file_manager, resolve_launch_target, start_install,
    stop_game,
};
use super::data;
use super::install::InstallRequest;
use super::shelf_popups;
use super::{LibraryModel, LibraryPopup, LibraryView};
use crate::capabilities::{Caps, testing_badge};
use crate::i18n::Text;
use crate::state::{AppState, DownloadKey, InstallStatus};
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
    pub(crate) focus: FocusHandle,
    pub(crate) naming: bool,
    pub(crate) name: Entity<TextInput>,
    /// Set on every render, since what Enter does depends on the popup's
    /// current target; Escape always just closes.
    pub(crate) on_submit: Option<Callback>,
    on_cancel: Option<Rc<dyn Fn(&mut App)>>,
    data: Option<MenuData>,
}

/// The game menu's sqlite reads, kept for as long as the menu is open.
#[derive(Clone)]
struct MenuData {
    organisation: u32,
    /// The library's revision: an install or uninstall finishing changes
    /// what the manifest reads below return.
    revision: u64,
    is_fav: bool,
    is_hidden: bool,
    collections: super::data::Collections,
    /// For a single game only: what Jugar would launch, and where its
    /// files are. Both are manifest reads.
    launch_target: Option<decanter_core::InstalledGame>,
    install_path: Option<std::path::PathBuf>,
}

impl PopupState {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let name = cx.new(|cx| TextInput::new(Text::new("library.menu.name_placeholder"), cx).text_size(12.));
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
            data: None,
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
    // Each product id in the target carries its own account (two accounts
    // in this profile can each own a copy), so favorites/hidden go through
    // that account, not a single one for the whole menu.
    let accounts: std::collections::HashMap<String, (String, Storefront)> = AppState::get(cx)
        .library
        .read(cx)
        .games
        .iter()
        .filter(|entry| target.product_ids.contains(&entry.game.id))
        .map(|entry| {
            (
                entry.game.id.clone(),
                (entry.game.account_id.clone(), entry.game.storefront),
            )
        })
        .collect();
    // Collections are per profile, but membership is still keyed by
    // storefront, so each id keeps the one it came from.
    let members: Rc<[(String, Storefront)]> = accounts
        .iter()
        .map(|(id, (_, storefront))| (id.clone(), *storefront))
        .collect();
    let account_for = move |product_id: &str| accounts.get(product_id).cloned();
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
    load_menu_data(
        &target.product_ids,
        profile_id,
        single_entry.as_ref(),
        &account_for,
        state,
        cx,
    );
    let Some(MenuData {
        is_fav,
        is_hidden,
        collections,
        launch_target,
        install_path,
        ..
    }) = state.read(cx).data.as_ref().map(MenuData::clone)
    else {
        return div().into_any_element();
    };

    let mut panel = menu::panel().child(menu::title(target.title.clone()));
    if let Some(entry) = single_entry.filter(|entry| entry.kind == GameKind::Game) {
        panel = panel
            .child(game_action_item(&entry, launch_target, close, cx))
            .children(open_folder_item(install_path, close))
            .children(edit_external_item(&entry, close))
            .child(properties_item(&entry, close))
            .child(menu::separator());
    }

    panel = panel
        .child(favorite_item(
            target.product_ids.clone(),
            account_for.clone(),
            is_fav,
            close,
        ))
        .child(hide_item(target.product_ids.clone(), account_for, is_hidden, close));

    panel = panel
        .child(menu::separator())
        .child(menu::label(&t!("library.menu.collections")));
    if collections.list.is_empty() {
        panel = panel.child(menu::empty(t!("library.menu.no_collections")));
    } else {
        panel = panel.child(collection_list(&collections, &target.product_ids, &members));
    }

    panel
        .child(new_collection_entry(profile_id, members, state, cx))
        .into_any_element()
}

fn load_menu_data(
    product_ids: &[String],
    profile_id: &str,
    single_entry: Option<&LibraryEntry>,
    account_for: &impl Fn(&str) -> Option<(String, Storefront)>,
    state: &Entity<PopupState>,
    cx: &mut Context<LibraryView>,
) {
    // Read once per change to favourites/collections, not on every frame
    // the menu is open (hovering its items redraws it).
    let organisation = AppState::get(cx).background.read(cx).organisation;
    let revision = AppState::get(cx).library.read(cx).revision;
    if state
        .read(cx)
        .data
        .as_ref()
        .is_none_or(|data| data.organisation != organisation || data.revision != revision)
    {
        // "Every one of them already is" decides which way a toggle goes,
        // so a mixed selection moves to the same state rather than
        // inverting each game independently.
        let data = MenuData {
            is_fav: product_ids.iter().all(|id| {
                account_for(id).is_some_and(|(account_id, storefront)| data::is_favorite(&account_id, storefront, id))
            }),
            is_hidden: product_ids.iter().all(|id| {
                account_for(id)
                    .is_some_and(|(account_id, storefront)| data::hidden_ids(&account_id, storefront).contains(id))
            }),
            collections: data::load_collections(profile_id),
            launch_target: single_entry
                .filter(|entry| entry.kind == GameKind::Game)
                .and_then(|entry| resolve_launch_target(&entry.game.account_id, &entry.game)),
            install_path: single_entry
                .filter(|entry| entry.kind == GameKind::Game)
                .and_then(|entry| game_install_path(&entry.game.account_id, &entry.game.id)),
            organisation,
            revision,
        };
        state.update(cx, |state, _| state.data = Some(data));
    }
}

fn favorite_item(
    ids: Vec<String>,
    accounts: impl Fn(&str) -> Option<(String, Storefront)> + 'static,
    is_fav: bool,
    close: &menu::Dismiss,
) -> Stateful<Div> {
    let close = Rc::clone(close);
    menu::item("menu-favorite", false)
        .on_click(move |_, window, cx| {
            tracing::info!(
                count = ids.len(),
                favorite = !is_fav,
                "bulk favorite toggle from context menu"
            );
            for product_id in &ids {
                if let Some((account_id, storefront)) = accounts(product_id) {
                    data::set_favorite(&account_id, storefront, product_id, !is_fav);
                }
            }
            bump_organisation(cx);
            close(window, cx);
        })
        .child(if is_fav {
            t!("library.detail.favorite.remove")
        } else {
            t!("library.detail.favorite.add")
        })
}

fn hide_item(
    ids: Vec<String>,
    accounts: impl Fn(&str) -> Option<(String, Storefront)> + 'static,
    is_hidden: bool,
    close: &menu::Dismiss,
) -> Stateful<Div> {
    let close = Rc::clone(close);
    menu::item("menu-hide", false)
        .on_click(move |_, window, cx| {
            tracing::info!(
                count = ids.len(),
                hidden = !is_hidden,
                "bulk hide toggle from context menu"
            );
            for product_id in &ids {
                if let Some((account_id, storefront)) = accounts(product_id) {
                    data::set_hidden(&account_id, storefront, product_id, !is_hidden);
                }
            }
            bump_organisation(cx);
            close(window, cx);
        })
        .child(if is_hidden {
            t!("library.menu.unhide")
        } else {
            t!("library.menu.hide")
        })
}

fn collection_list(
    collections: &data::Collections,
    product_ids: &[String],
    members: &Rc<[(String, Storefront)]>,
) -> Stateful<Div> {
    let items = collections.list.iter().enumerate().map(|(index, collection)| {
        let member = product_ids.iter().all(|id| collections.contains(&collection.id, id));
        // A dynamic collection is defined by its filter; adding a game
        // by hand would be a change the next render throws away.
        let dynamic = collection.filter.is_some();
        let members = Rc::clone(members);
        let collection_id = collection.id.clone();
        menu::item(("menu-collection", index), dynamic)
            .when(dynamic, |item| {
                item.tooltip(tooltip(t!("library.menu.dynamic_collection")))
            })
            .when(!dynamic, |item| {
                item.on_click(move |_, _, cx| {
                    for (product_id, storefront) in members.iter() {
                        data::set_collection_member(&collection_id, *storefront, product_id, !member);
                    }
                    bump_organisation(cx);
                })
            })
            .child(menu::check(member))
            .child(collection.name.clone())
            .when(dynamic, |item| item.child(div().ml_auto().child(auto_tag("auto"))))
    });
    div()
        .id("menu-collections")
        .max_h(px(200.))
        .overflow_y_scroll()
        .children(items)
}

fn new_collection_entry(
    profile_id: &str,
    members: Rc<[(String, Storefront)]>,
    state: &Entity<PopupState>,
    cx: &mut Context<LibraryView>,
) -> AnyElement {
    if state.read(cx).naming {
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
                    for (product_id, storefront) in members.iter() {
                        data::set_collection_member(&created.id, *storefront, product_id, true);
                    }
                }
                name.update(cx, |input, cx| input.set_text("", cx));
                bump_organisation(cx);
            }));
        });
        div().pt_1().child(state.read(cx).name.clone()).into_any_element()
    } else {
        let state = state.clone();
        menu::item("menu-new-collection", false)
            .on_click(move |_, window, cx| {
                state.update(cx, |state, cx| {
                    state.naming = true;
                    window.focus(&state.name.focus_handle(cx), cx);
                    cx.notify();
                });
            })
            .child(t!("library.menu.new_collection"))
            .into_any_element()
    }
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
fn game_action_item(
    entry: &LibraryEntry,
    launch_target: Option<decanter_core::InstalledGame>,
    close: &menu::Dismiss,
    cx: &App,
) -> AnyElement {
    let account_id = entry.game.account_id.clone();
    let background = AppState::get(cx).background.read(cx);
    let is_downloading = background
        .status(&entry.game.account_id, &entry.game.id)
        .is_some_and(InstallStatus::is_live);
    let running = background.running.contains(&entry.game.id);
    let is_installed = entry.game.install_state.as_ref().is_some_and(|state| state.installed);
    let close = Rc::clone(close);
    let product_id = entry.game.id.clone();

    // Descargas asks the question, with how much would be deleted.
    if is_downloading {
        return menu::item("menu-action", false)
            .on_click(move |_, window, cx| {
                super::actions::ask_cancel(DownloadKey::new(&account_id, &product_id), cx);
                crate::views::downloads::open(cx);
                close(window, cx);
            })
            .child(t!("library.menu.cancel_install"))
            .into_any_element();
    }

    if launch_target.is_none()
        && let Some(reason) = super::actions::missing_build(&entry.game)
    {
        return menu::item("menu-action", false)
            .opacity(0.5)
            .cursor_default()
            .hover(|item| item)
            .child(reason)
            .into_any_element();
    }

    if !is_installed {
        // Failed sits here too: whatever partial state it left behaves the
        // same as never having started — retrying is just installing again.
        let title = entry.game.title.clone();
        return menu::item("menu-action", false)
            .on_click(move |_, window, cx| {
                start_install(
                    InstallRequest {
                        account_id: account_id.clone(),
                        product_id: product_id.clone(),
                        parent_id: None,
                        title: title.clone(),
                        branch: None,
                        dlcs: Vec::new(),
                        library_root: None,
                    },
                    cx,
                );
                close(window, cx);
            })
            .child(t!("library.menu.install"))
            .into_any_element();
    }

    let Some(installed) = launch_target else {
        return menu::item("menu-action", false)
            .opacity(0.5)
            .cursor_default()
            .hover(|item| item)
            .child(t!("library.detail.no_executable"))
            .into_any_element();
    };
    let testing = match Caps::get(cx).support(Feature::Play) {
        Support::Unavailable(_) => {
            return menu::item("menu-action", false)
                .opacity(0.5)
                .cursor_default()
                .hover(|item| item)
                .child(t!("library.detail.play_unavailable"))
                .into_any_element();
        }
        Support::Available => false,
        Support::Testing => true,
    };

    let item = if running {
        menu::item("menu-action", false)
            .on_click(move |_, window, cx| {
                stop_game(account_id.clone(), product_id.clone(), cx);
                close(window, cx);
            })
            .child(t!("library.menu.stop"))
    } else {
        menu::item("menu-action", false)
            .on_click(move |_, window, cx| {
                launch_game(account_id.clone(), installed.clone(), cx);
                close(window, cx);
            })
            .child(t!("library.menu.play"))
    };
    item.when(testing, |item| item.child(testing_badge()))
        .into_any_element()
}

/// Opens the install directory — needs only the files, not a resolved
/// launch executable, so it also shows in the "sin ejecutable" case.
fn open_folder_item(install_path: Option<std::path::PathBuf>, close: &menu::Dismiss) -> Option<AnyElement> {
    let Some(path) = install_path else {
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
            .child(t!("library.menu.open_folder"))
            .into_any_element(),
    )
}

/// Steam's right-click → Propiedades, the same window the detail pane's
/// own button opens.
/// An external game's name, executable and art are edited where it was
/// added, not in Propiedades.
fn edit_external_item(entry: &LibraryEntry, close: &menu::Dismiss) -> Option<AnyElement> {
    if entry.game.storefront != Storefront::Unspecified {
        return None;
    }
    let id = entry.game.id.clone();
    let close = Rc::clone(close);
    Some(
        menu::item("menu-edit-external", false)
            .on_click(move |_, window, cx| {
                close(window, cx);
                crate::views::external::open(Some(id.clone()), cx);
            })
            .child(t!("library.menu.edit_external"))
            .into_any_element(),
    )
}

fn properties_item(entry: &LibraryEntry, close: &menu::Dismiss) -> AnyElement {
    let game = entry.game.clone();
    let close = Rc::clone(close);
    menu::item("menu-properties", false)
        .on_click(move |_, window, cx| {
            close(window, cx);
            super::open_properties(&game, cx);
        })
        .child(t!("library.properties.title"))
        .into_any_element()
}
