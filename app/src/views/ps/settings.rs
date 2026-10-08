//! Ajustes of the PS section: the folders this profile's dumps are read from.

use gpui::{Context, Entity, SharedString, Window, div, prelude::*};

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub struct SettingsView {
    ps: Entity<Ps>,
}

impl SettingsView {
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |_, _, cx| cx.notify()).detach();
        Self { ps }
    }

    fn pick_folder(&self, cx: &mut Context<Self>) {
        let ps = self.ps.clone();
        cx.spawn(async move |_, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title("Elegí una carpeta con volcados de PS4 o PS5")
                .pick_folder()
                .await
            else {
                return;
            };
            let path = folder.path().to_string_lossy().into_owned();
            ps.update(cx, |ps, cx| ps.add_dir(path, cx));
        })
        .detach();
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dirs = self.ps.read(cx).dirs().to_vec();
        let count = dirs.len();
        let list = if dirs.is_empty() {
            theme::empty("Todavía no hay carpetas.").into_any_element()
        } else {
            theme::card()
                .children(dirs.into_iter().enumerate().map(|(index, dir)| {
                    let ps = self.ps.clone();
                    let remove = dir.clone();
                    theme::list_row(("ps-dir", index), index + 1 == count)
                        .child(
                            theme::mono()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(SharedString::from(dir)),
                        )
                        .child(
                            button(("ps-dir-remove", index), ButtonKind::Icon, "Quitar", false).on_click(
                                move |_, _, cx| {
                                    let remove = remove.clone();
                                    ps.update(cx, |ps, cx| ps.remove_dir(remove, cx));
                                },
                            ),
                        )
                }))
                .into_any_element()
        };
        theme::page(
            "ps-settings",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(theme::h2("Carpetas de juegos"))
                .child(theme::muted(
                    "Cada carpeta y sus subcarpetas directas se leen tal cual: decanter no mueve ni copia \
                     los volcados.",
                ))
                .child(list)
                .child(
                    div().flex().child(
                        button("ps-dir-add", ButtonKind::Secondary, "Añadir carpeta", false)
                            .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
                    ),
                ),
        )
    }
}
