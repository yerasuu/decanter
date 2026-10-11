//! The right-click menu of a PS game, on its sidebar row and its cover: what
//! the game's own page offers, without opening it.

use std::path::PathBuf;
use std::rc::Rc;

use decanter_ps::status::Status;
use gpui::{AnyElement, App, ClipboardItem, Entity, Pixels, Point, Window, prelude::*};

use super::detail::DetailView;
use super::display_title;
use crate::state::{JobKey, Ps};
use crate::widgets::menu;

/// Which game the open menu is for, and where it was asked for.
pub struct GameMenu {
    pub dir: PathBuf,
    pub position: Point<Pixels>,
}

/// `None` once the game left the list (a rescan while the menu was open).
pub fn render(
    target: &GameMenu,
    ps: &Entity<Ps>,
    detail: &Entity<DetailView>,
    window: &Window,
    dismiss: menu::Dismiss,
    cx: &App,
) -> Option<AnyElement> {
    let state = ps.read(cx);
    let entry = state.entries().iter().find(|entry| entry.game.dir == target.dir)?;
    let key = JobKey::Game(target.dir.clone());
    let running = state.is_running(&key, cx);
    let log_file = state.job(&key).and_then(|job| job.read(cx).log_file.clone());

    let mut panel = menu::panel().child(menu::title(display_title(&entry.game)));

    if running {
        let (ps, key, close) = (ps.clone(), key.clone(), Rc::clone(&dismiss));
        panel = panel.child(
            menu::item("ps-menu-stop", false)
                .child("Detener")
                .on_click(move |_, window, cx| {
                    ps.update(cx, |ps, cx| ps.stop(&key, cx));
                    close(window, cx);
                }),
        );
    } else {
        let playable = entry.status.playable();
        let (ps, dir, close) = (ps.clone(), target.dir.clone(), Rc::clone(&dismiss));
        panel = panel.child(
            menu::item("ps-menu-play", !playable)
                .child("Jugar")
                .when(playable, |item| {
                    item.on_click(move |_, window, cx| {
                        ps.update(cx, |ps, cx| ps.play(&dir, cx));
                        close(window, cx);
                    })
                }),
        );
    }

    {
        let (detail, dir, close) = (detail.clone(), target.dir.clone(), Rc::clone(&dismiss));
        panel = panel.child(menu::item("ps-menu-details", false).child("Ver detalles").on_click(
            move |_, window, cx| {
                detail.update(cx, |detail, cx| detail.select(dir.clone(), cx));
                close(window, cx);
            },
        ));
    }

    if let Status::NeedsMerge(update) = &entry.status {
        let (ps, base, update, close) = (ps.clone(), target.dir.clone(), update.clone(), Rc::clone(&dismiss));
        panel = panel.child(
            menu::item("ps-menu-merge", false)
                .child("Fusionar actualización 1.09")
                .on_click(move |_, window, cx| {
                    ps.update(cx, |ps, cx| ps.merge(base.clone(), update.clone(), cx));
                    close(window, cx);
                }),
        );
    }

    panel = panel.child(menu::separator());

    {
        let close = Rc::clone(&dismiss);
        panel = panel.child(
            menu::item("ps-menu-log", log_file.is_none())
                .child("Abrir log completo")
                .when_some(log_file, |item, path| {
                    item.on_click(move |_, window, cx| {
                        if let Err(err) = crate::desktop::open(&path) {
                            tracing::warn!(path = %path.display(), %err, "could not open the log file");
                        }
                        close(window, cx);
                    })
                }),
        );
    }

    {
        let (dir, close) = (target.dir.clone(), Rc::clone(&dismiss));
        panel = panel.child(
            menu::item("ps-menu-folder", false)
                .child("Abrir carpeta del volcado")
                .on_click(move |_, window, cx| {
                    if let Err(err) = crate::desktop::open(&dir) {
                        tracing::warn!(dir = %dir.display(), %err, "could not open the folder");
                    }
                    close(window, cx);
                }),
        );
    }

    {
        let (title_id, close) = (entry.game.title_id.clone(), Rc::clone(&dismiss));
        panel = panel.child(menu::item("ps-menu-title-id", false).child("Copiar Title ID").on_click(
            move |_, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(title_id.clone()));
                close(window, cx);
            },
        ));
    }

    Some(menu::popup(target.position, panel, window, dismiss))
}
