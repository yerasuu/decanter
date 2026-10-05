//! Non-Steam-style shortcuts: an arbitrary executable, added by hand, that
//! runs through the same runner as a GOG install — its own prefix, its
//! own Proton pin — but with no catalog entry behind it at all.
//!
//! Deliberately not folded into the GOG library: there is no product id
//! to fetch metadata for, no depot to verify against, nothing decanter
//! installed. It is just a path someone pointed at.
//!
//! Filed under the profile itself rather than one of its accounts — a
//! shortcut isn't owned by any storefront, so there's no account to pick,
//! and unlike a GOG game it never needs to show up twice for two accounts
//! that both own it.

use decanter_core::InstalledGame;
use gpui::{App, Context, Entity, FontWeight, IntoElement, Window, div, prelude::*};

use super::{column, field_row};
use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, TextInput, button, tooltip};

pub struct ShortcutsTab {
    profile_id: String,
    shortcuts: Vec<InstalledGame>,
    name: Entity<TextInput>,
    path: Entity<TextInput>,
    error: String,
}

impl ShortcutsTab {
    pub fn new(profile_id: String, cx: &mut Context<Self>) -> Self {
        // `running` swaps a row's JUGAR for DETENER and locks its Eliminar.
        let background = AppState::get(cx).background.clone();
        cx.observe(&background, |_, _, cx| cx.notify()).detach();
        Self {
            shortcuts: crate::core::shortcuts(&profile_id),
            profile_id,
            name: cx.new(|cx| TextInput::new("Nombre", cx)),
            path: cx.new(|cx| TextInput::new("Ejecutable", cx)),
            error: String::new(),
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.shortcuts = crate::core::shortcuts(&self.profile_id);
        cx.notify();
    }

    /// Native file picker (XDG portal), not a bare text field the user has
    /// to type a path into — that's how Steam does it, and typos in an
    /// absolute path are otherwise a silent "no lo pudo abrir" later.
    fn pick_file(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Elegi el ejecutable")
                .pick_file()
                .await
            else {
                return;
            };
            let chosen = file.path().to_path_buf();
            tracing::debug!(path = %chosen.display(), "shortcut executable picked from file dialog");
            let _ = this.update(cx, |this, cx| {
                if this.name.read(cx).text().trim().is_empty()
                    && let Some(stem) = chosen.file_stem().and_then(|s| s.to_str())
                {
                    this.name.update(cx, |input, cx| input.set_text(stem.to_string(), cx));
                }
                this.path.update(cx, |input, cx| {
                    input.set_text(chosen.to_string_lossy().into_owned(), cx)
                });
            });
        })
        .detach();
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_string();
        let path = self.path.read(cx).text().trim().to_string();
        cx.notify();
        if name.is_empty() || path.is_empty() {
            self.error = "Poné un nombre y un ejecutable.".to_string();
            return;
        }
        let executable = std::path::PathBuf::from(&path);
        if !executable.is_file() {
            self.error = "Ese archivo no existe.".to_string();
            return;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let profile_id = self.profile_id.as_str();
        match crate::core::add_shortcut(profile_id, &id, &name, &executable) {
            Ok(()) => {
                tracing::info!(profile_id, shortcut_id = id, name, "shortcut added");
                self.name.update(cx, |input, cx| input.set_text("", cx));
                self.path.update(cx, |input, cx| input.set_text("", cx));
                self.error.clear();
                self.reload(cx);
            }
            Err(err) => {
                tracing::error!(profile_id, name, error = %err, "failed to add shortcut");
                self.error = err;
            }
        }
    }

    fn remove(&mut self, game_id: &str, cx: &mut Context<Self>) {
        let profile_id = self.profile_id.as_str();
        match crate::core::remove_shortcut(profile_id, game_id) {
            Ok(()) => tracing::info!(profile_id, shortcut_id = game_id, "shortcut removed"),
            Err(err) => tracing::error!(profile_id, shortcut_id = game_id, %err, "no se pudo eliminar el atajo"),
        }
        self.reload(cx);
    }

    fn row(&self, index: usize, game: &InstalledGame, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let is_running = AppState::get(cx).background.read(cx).running.contains(&game.game_id);
        let profile_id = self.profile_id.clone();
        let action = if is_running {
            let game_id = game.game_id.clone();
            button(("stop-shortcut", index), ButtonKind::Secondary, "DETENER", false).on_click(
                move |_, _, cx: &mut App| {
                    tracing::info!(profile_id, product_id = game_id, "stopping shortcut");
                    let (profile_id, game_id) = (profile_id.clone(), game_id.clone());
                    cx.spawn(async move |_| crate::launch::stop(profile_id, game_id).await)
                        .detach();
                },
            )
        } else {
            let game = game.clone();
            button(("play-shortcut", index), ButtonKind::Primary, "JUGAR", false).on_click(move |_, _, cx: &mut App| {
                tracing::info!(profile_id, product_id = game.game_id, "launching shortcut");
                // App-level, not this view's: the session outlives
                // Ajustes being closed, and it writes `running` itself.
                let background = AppState::get(cx).background.clone();
                let (account_id, game) = (profile_id.clone(), game.clone());
                cx.spawn(async move |cx| {
                    crate::launch::start(account_id, game, background, cx).await;
                })
                .detach();
            })
        };
        let game_id = game.game_id.clone();
        theme::list_row(("shortcut-row", index), index + 1 == self.shortcuts.len())
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(game.name.clone()),
            )
            .child(theme::muted(game.launch_executable.clone().unwrap_or_default()))
            .child(action)
            .child(
                button(("remove-shortcut", index), ButtonKind::Icon, "Eliminar", is_running)
                    .tooltip(tooltip(if is_running {
                        "Deten\u{e9} el juego antes de eliminarlo"
                    } else {
                        "Eliminar"
                    }))
                    .when(!is_running, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.remove(&game_id, cx)))
                    }),
            )
    }
}

impl Render for ShortcutsTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list = if self.shortcuts.is_empty() {
            theme::empty("No hay juegos externos todavia.").into_any_element()
        } else {
            theme::card()
                .children(
                    self.shortcuts
                        .iter()
                        .enumerate()
                        .map(|(index, game)| self.row(index, game, cx)),
                )
                .into_any_element()
        };

        column()
            .child(theme::muted(
                "Ejecutables que no vinieron de GOG — cada uno corre en su propio prefijo, igual que un juego instalado.",
            ))
            .child(theme::h2("Agregar"))
            .child(
                field_row()
                    .child(div().flex_1().min_w_0().child(self.name.clone()))
                    .child(div().flex_1().min_w_0().child(self.path.clone()))
                    .child(
                        button("pick-shortcut", ButtonKind::Secondary, "Elegir archivo...", false)
                            .on_click(cx.listener(|this, _, _, cx| this.pick_file(cx))),
                    )
                    .child(
                        button("add-shortcut", ButtonKind::Primary, "Agregar", false)
                            .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                    ),
            )
            .when(!self.error.is_empty(), |column| column.child(theme::error(self.error.clone())))
            .child(list)
    }
}
