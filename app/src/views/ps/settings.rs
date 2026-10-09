//! Ajustes of the PS section: where this profile's dumps are read from.

use gpui::{Context, Entity, Window, div, prelude::*};

use crate::state::Ps;
use crate::theme;
use crate::widgets::{ButtonKind, button};

pub struct SettingsView;

impl SettingsView {
    /// Watches `ps` since the games folder follows the active profile's
    /// library.
    pub fn new(ps: Entity<Ps>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ps, |_, _, cx| cx.notify()).detach();
        Self
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let games = decanter_ps::paths::games();
        theme::page(
            "ps-settings",
            div()
                .flex()
                .flex_col()
                .gap_4()
                .child(theme::h2("Carpeta de juegos"))
                .child(
                    theme::card().child(
                        theme::list_row("ps-dir-library", true)
                            .child(
                                theme::mono()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(games.display().to_string()),
                            )
                            .child(button("ps-dir-open", ButtonKind::Secondary, "Abrir", false).on_click(
                                move |_, _, _| {
                                    let _ = std::fs::create_dir_all(&games);
                                    if let Err(err) = crate::desktop::open(&games) {
                                        tracing::warn!(dir = %games.display(), %err, "could not open the folder");
                                    }
                                },
                            )),
                    ),
                )
                .child(theme::muted(
                    "Junto a la biblioteca del perfil, en el mismo disco. Copiá o mové ahí tus volcados y \
                     aparecen solos: cada carpeta y sus subcarpetas directas se leen tal cual.",
                )),
        )
    }
}
