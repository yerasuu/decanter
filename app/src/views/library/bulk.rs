//! What the game menu offers for a selection of several games: each action applies to the
//! selected games it makes sense for, and is left out when none of them qualifies.

use std::rc::Rc;

use decanter_core::Storefront;
use decanter_gog::GameKind;
use gpui::{AnyElement, App, Context, Entity, Window, div, prelude::*, px};
use rust_i18n::t;

use super::actions::bump_organisation;
use super::install::{self, InstallRequest};
use super::install_dialog::{dialog_frame, dialog_header};
use super::{LibraryModel, LibraryView};
use crate::i18n::Text;
use crate::mods::OnUninstall;
use crate::state::{AppState, InstallStatus};
use crate::theme;
use crate::widgets::{ButtonKind, button, menu};

/// Only the stores decanter installs from have an install and an uninstall to offer; a Steam or
/// external entry is only ever added or removed from its own page.
const fn is_managed_store(storefront: Storefront) -> bool {
    matches!(
        storefront,
        Storefront::Gog | Storefront::EpicGames | Storefront::AmazonGames
    )
}

fn is_installed(entry: &decanter_gog::LibraryEntry) -> bool {
    entry.game.install_state.as_ref().is_some_and(|state| state.installed)
}

fn selected<'games>(
    product_ids: &'games [String],
    cx: &'games App,
) -> impl Iterator<Item = &'games decanter_gog::LibraryEntry> + 'games {
    AppState::get(cx)
        .library
        .read(cx)
        .games
        .iter()
        .filter(move |entry| entry.kind == GameKind::Game && product_ids.contains(&entry.game.id))
}

/// Instalar for the selected games not installed and not already downloading: they all go into
/// one install dialog.
pub fn install_item(
    product_ids: &[String],
    close: &menu::Dismiss,
    model: &Entity<LibraryModel>,
    cx: &App,
) -> Option<AnyElement> {
    let background = AppState::get(cx).background.read(cx);
    let requests: Vec<InstallRequest> = selected(product_ids, cx)
        .filter(|entry| is_managed_store(entry.game.storefront) && !is_installed(entry))
        .filter(|entry| {
            !background
                .status(&entry.game.account_id, &entry.game.id)
                .is_some_and(InstallStatus::is_live)
        })
        .filter(|entry| super::actions::missing_build(&entry.game).is_none())
        .map(|entry| super::context_menu::plain_install(&entry.game.account_id, &entry.game.id, &entry.game.title))
        .collect();
    if requests.is_empty() {
        return None;
    }
    let label = t!("library.menu.install_many", count = requests.len());
    let (close, model) = (Rc::clone(close), model.clone());
    let requests = Rc::new(requests);
    Some(
        menu::item("menu-install-many", false)
            .on_click(move |_, window, cx| {
                let requests = requests
                    .iter()
                    .map(|request| {
                        super::context_menu::plain_install(&request.account_id, &request.product_id, &request.title)
                    })
                    .collect();
                model.update(cx, |model, cx| model.request_install(requests, cx));
                close(window, cx);
            })
            .child(label)
            .into_any_element(),
    )
}

#[derive(Clone)]
pub struct Target {
    account_id: String,
    product_id: String,
    title: String,
}

enum Phase {
    Asking,
    Running,
    Done(Vec<(String, Text)>),
}

/// The confirmation for uninstalling several games at once, its progress and what failed, kept in
/// the model so the menu that opened it can close.
pub struct Uninstall {
    targets: Vec<Target>,
    phase: Phase,
}

/// Desinstalar for the selected games that are installed. A game with a download running is left
/// out: stopping it is a cancel, which asks its own question.
pub fn uninstall_item(
    product_ids: &[String],
    close: &menu::Dismiss,
    model: &Entity<LibraryModel>,
    cx: &App,
) -> Option<AnyElement> {
    let background = AppState::get(cx).background.read(cx);
    let targets: Vec<Target> = selected(product_ids, cx)
        .filter(|entry| is_managed_store(entry.game.storefront) && is_installed(entry))
        .filter(|entry| {
            !background
                .status(&entry.game.account_id, &entry.game.id)
                .is_some_and(InstallStatus::is_live)
        })
        .map(|entry| Target {
            account_id: entry.game.account_id.clone(),
            product_id: entry.game.id.clone(),
            title: entry.game.title.clone(),
        })
        .collect();
    if targets.is_empty() {
        return None;
    }
    let label = t!("library.menu.uninstall_many", count = targets.len());
    let (close, model) = (Rc::clone(close), model.clone());
    Some(
        menu::item("menu-uninstall-many", false)
            .on_click(move |_, window, cx| {
                let targets = targets.clone();
                model.update(cx, |model, cx| {
                    model.uninstall = Some(Uninstall {
                        targets,
                        phase: Phase::Asking,
                    });
                    cx.notify();
                });
                close(window, cx);
            })
            .child(label)
            .into_any_element(),
    )
}

/// The confirmation modal, with its progress and its failures once it has run.
pub fn uninstall_modal(
    model: &Entity<LibraryModel>,
    window: &Window,
    cx: &mut Context<LibraryView>,
) -> Option<AnyElement> {
    let model_read = model.read(cx);
    let uninstall = model_read.uninstall.as_ref()?;
    let running = matches!(uninstall.phase, Phase::Running);
    let close: menu::Dismiss = {
        let model = model.clone();
        Rc::new(move |_, cx| {
            // Closing mid-way would hide the failures of a run that goes on anyway.
            model.update(cx, |model, cx| {
                if !matches!(
                    model.uninstall.as_ref().map(|uninstall| &uninstall.phase),
                    Some(Phase::Running)
                ) {
                    model.uninstall = None;
                    cx.notify();
                }
            });
        })
    };
    let title = t!("library.menu.uninstall_many", count = uninstall.targets.len()).into_owned();
    let mut body = div()
        .id("bulk-uninstall-body")
        .flex()
        .flex_col()
        .gap_2()
        .overflow_y_scroll()
        .pt(px(20.))
        .px_6()
        .pb_6();
    match &uninstall.phase {
        Phase::Asking => {
            let start = {
                let model = model.clone();
                move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| run(&model, cx)
            };
            let cancel = Rc::clone(&close);
            body = body
                .children(uninstall.targets.iter().map(|target| div().child(target.title.clone())))
                .child(theme::muted(t!("library.menu.uninstall_many_note")))
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            button(
                                "bulk-uninstall-confirm",
                                ButtonKind::Primary,
                                t!("library.menu.uninstall_many_confirm"),
                                false,
                            )
                            .on_click(start),
                        )
                        .child(
                            button(
                                "bulk-uninstall-cancel",
                                ButtonKind::Secondary,
                                t!("common.cancel"),
                                false,
                            )
                            .on_click(move |_, window, cx| cancel(window, cx)),
                        ),
                );
        }
        Phase::Running => {
            body = body.child(theme::muted(t!("library.detail.uninstall.busy")));
        }
        Phase::Done(failures) => {
            let done = Rc::clone(&close);
            body = body
                .child(theme::muted(t!(
                    "library.menu.uninstall_many_failed",
                    count = failures.len()
                )))
                .children(
                    failures
                        .iter()
                        .map(|(title, err)| theme::error(format!("{title}: {err}"))),
                )
                .child(
                    div().flex().child(
                        button(
                            "bulk-uninstall-close",
                            ButtonKind::Secondary,
                            t!("library.menu.uninstall_many_close"),
                            false,
                        )
                        .on_click(move |_, window, cx| done(window, cx)),
                    ),
                );
        }
    }
    let dismiss = if running {
        Rc::new(|_: &mut Window, _: &mut App| {})
    } else {
        Rc::clone(&close)
    };
    let panel = dialog_frame(window).child(dialog_header(&title, close)).child(body);
    Some(menu::modal(panel, window, dismiss))
}

/// One game after another through the single-game uninstall, so a failure on one is kept for the
/// report and the rest still go. Detached from the modal: it deletes real files, so closing the
/// window of the menu mustn't stop it half way.
fn run(model: &Entity<LibraryModel>, cx: &mut App) {
    let targets = model.update(cx, |model, cx| {
        let Some(uninstall) = model.uninstall.as_mut() else {
            return Vec::new();
        };
        uninstall.phase = Phase::Running;
        cx.notify();
        uninstall.targets.clone()
    });
    let state = AppState::get(cx).clone();
    let model = model.downgrade();
    cx.spawn(async move |cx| {
        let mut failures = Vec::new();
        for target in targets {
            let result = install::uninstall(
                target.account_id.clone(),
                target.product_id.clone(),
                OnUninstall::KeepMods,
                state.clone(),
                cx,
            )
            .await;
            if let Err(err) = result {
                tracing::error!(
                    account_id = %target.account_id,
                    product_id = %target.product_id,
                    %err,
                    "uninstall of a selected game failed"
                );
                failures.push((target.title, err));
            }
        }
        let _ = model.update(cx, |model, cx| {
            model.uninstall = (!failures.is_empty()).then(|| Uninstall {
                targets: Vec::new(),
                phase: Phase::Done(failures),
            });
            cx.notify();
        });
    })
    .detach();
}

#[derive(Clone)]
pub struct SteamTarget {
    account_id: String,
    product_id: String,
    art: (Option<String>, Option<String>, Option<String>),
}

/// The selected installed GOG games split by whether they already have a Steam shortcut, so the
/// menu can offer to add the ones without and remove the ones with.
#[derive(Clone, Default)]
pub struct SteamSelection {
    add: Vec<SteamTarget>,
    remove: Vec<SteamTarget>,
}

pub fn steam_selection(product_ids: &[String], cx: &App) -> SteamSelection {
    let Ok(db) = crate::core::db() else {
        return SteamSelection::default();
    };
    let can_add = decanter_core::steam::steam_root().is_some();
    let mut selection = SteamSelection::default();
    for entry in
        selected(product_ids, cx).filter(|entry| entry.game.storefront == Storefront::Gog && is_installed(entry))
    {
        let exported = db
            .steam_shortcut_appid(&entry.game.account_id, Storefront::Gog, &entry.game.id)
            .is_ok_and(|appid| appid.is_some());
        let target = SteamTarget {
            account_id: entry.game.account_id.clone(),
            product_id: entry.game.id.clone(),
            art: (
                entry.game.cover_url.clone(),
                entry.game.background_url.clone(),
                entry.game.icon_url.clone(),
            ),
        };
        if exported {
            selection.remove.push(target);
        } else if can_add {
            selection.add.push(target);
        }
    }
    selection
}

pub fn steam_items(selection: &SteamSelection, close: &menu::Dismiss) -> Vec<AnyElement> {
    let add = (!selection.add.is_empty()).then(|| {
        steam_item(
            "menu-steam-add-many",
            t!("library.menu.steam_add_many", count = selection.add.len()).into_owned(),
            selection.add.clone(),
            true,
            close,
        )
    });
    let remove = (!selection.remove.is_empty()).then(|| {
        steam_item(
            "menu-steam-remove-many",
            t!("library.menu.steam_remove_many", count = selection.remove.len()).into_owned(),
            selection.remove.clone(),
            false,
            close,
        )
    });
    add.into_iter().chain(remove).collect()
}

fn steam_item(
    id: &'static str,
    label: String,
    targets: Vec<SteamTarget>,
    add: bool,
    close: &menu::Dismiss,
) -> AnyElement {
    let close = Rc::clone(close);
    menu::item(id, false)
        .on_click(move |_, window, cx| {
            let targets = targets.clone();
            cx.spawn(async move |cx| {
                for target in targets {
                    if add {
                        if let Ok(db) = crate::core::db() {
                            install::export_steam_shortcut(&db, &target.account_id, &target.product_id, target.art)
                                .await;
                        }
                    } else {
                        install::remove_steam_shortcut_for(&target.account_id, &target.product_id).await;
                    }
                }
                cx.update(bump_organisation);
            })
            .detach();
            close(window, cx);
        })
        .child(label)
        .into_any_element()
}
