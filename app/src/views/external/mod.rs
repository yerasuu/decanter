//! "Juegos externos": the page behind Biblioteca's "+ Agregar un juego",
//! laid out like Ajustes — tabs for adding (or editing) one, the profile's
//! list of them, and where the art search gets its SteamGridDB key. Off the
//! topbar's nav, the same as Ajustes: reached from the sidebar's button or
//! a game's right-click "Editar".

mod form;

use decanter_core::InstalledGame;
use gpui::{App, Context, Entity, FontWeight, ObjectFit, Subscription, Window, div, img, prelude::*, px, rgb};

use crate::external::art;
use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, TextInput, button, tooltip};
use form::{FormEvent, GameForm};

/// Asks the shell to show this page, on a fresh form or on `editing`'s id.
pub fn open(editing: Option<String>, cx: &mut App) {
    let session = AppState::get(cx).session.clone();
    session.update(cx, |session, cx| {
        session.external_request = session.external_request.wrapping_add(1);
        session.external_edit = editing;
        cx.notify();
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Form,
    Games,
    Sources,
}

pub struct ExternalView {
    tab: Tab,
    profile_id: Option<String>,
    games: Vec<InstalledGame>,
    form: Option<(Entity<GameForm>, Subscription)>,
    api_key: Entity<TextInput>,
    key_status: String,
}

impl ExternalView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        // `running` locks a running game's Eliminar.
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.session, |this, session, cx| {
            let profile = session.read(cx).active_profile.clone();
            if profile != this.profile_id {
                this.start(None, cx);
            }
        })
        .detach();
        let api_key = cx.new(|cx| TextInput::new("API key de SteamGridDB", cx));
        // A keyring round trip: off the UI thread, filled in when it lands.
        let field = api_key.clone();
        cx.spawn(async move |_, cx| {
            let key = tokio::task::spawn_blocking(art::api_key).await.ok().flatten();
            if let Some(key) = key {
                let _ = field.update(cx, |input, cx| input.set_text(key, cx));
            }
        })
        .detach();
        let mut this = Self {
            tab: Tab::Form,
            profile_id: None,
            games: Vec::new(),
            form: None,
            api_key,
            key_status: String::new(),
        };
        this.start(None, cx);
        this
    }

    /// Swaps in a fresh form for the active profile — empty, or on the
    /// external game `editing` names — and shows it.
    pub fn start(&mut self, editing: Option<String>, cx: &mut Context<Self>) {
        self.profile_id = AppState::get(cx).session.read(cx).active_profile.clone();
        self.games = self
            .profile_id
            .as_deref()
            .map(crate::external::list)
            .unwrap_or_default();
        self.form = self.profile_id.clone().map(|profile_id| {
            let editing = editing.and_then(|id| self.games.iter().find(|game| game.game_id == id).cloned());
            let form = cx.new(|cx| GameForm::new(profile_id, editing, cx));
            let subscription = cx.subscribe(&form, |this, _, event, cx| match event {
                FormEvent::Saved => {
                    this.reload(cx);
                    this.start(None, cx);
                    this.tab = Tab::Games;
                }
            });
            (form, subscription)
        });
        self.tab = Tab::Form;
        cx.notify();
    }

    /// Re-reads the list here and the library's copy of it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.games = self
            .profile_id
            .as_deref()
            .map(crate::external::list)
            .unwrap_or_default();
        AppState::get(cx)
            .library
            .clone()
            .update(cx, |library, cx| library.refresh_external(cx));
        cx.notify();
    }

    fn editing(&self, cx: &App) -> bool {
        self.form.as_ref().is_some_and(|(form, _)| {
            let form = form.read(cx);
            self.games.iter().any(|game| form.is_editing(&game.game_id))
        })
    }

    fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(profile_id) = self.profile_id.as_deref() else {
            return;
        };
        if let Err(err) = crate::external::remove(profile_id, id) {
            tracing::error!(profile_id, id, %err, "could not remove external game");
        }
        if self.form.as_ref().is_some_and(|(form, _)| form.read(cx).is_editing(id)) {
            self.start(None, cx);
            self.tab = Tab::Games;
        }
        self.reload(cx);
    }

    fn save_key(&mut self, cx: &mut Context<Self>) {
        let key = self.api_key.read(cx).text().trim().to_owned();
        self.key_status = "Guardando...".to_owned();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = tokio::task::spawn_blocking(move || art::set_api_key(&key))
                .await
                .unwrap_or_else(|err| Err(err.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.key_status = match result {
                    Ok(()) => "Guardada.".to_owned(),
                    Err(err) => err,
                };
                cx.notify();
            });
        })
        .detach();
    }

    fn game_list(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        if self.games.is_empty() {
            return theme::empty("Todavía no agregaste ningún juego externo.").into_any_element();
        }
        let running = AppState::get(cx).background.read(cx).running.clone();
        let count = self.games.len();
        theme::card()
            .children(self.games.iter().enumerate().map(|(index, game)| {
                let is_running = running.contains(&game.game_id);
                let edit_id = game.game_id.clone();
                let id = game.game_id.clone();
                theme::list_row(("external-row", index), index + 1 == count)
                    .child(icon(game.art.icon.as_deref()))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(game.name.clone()))
                            .child(theme::muted(game.launch_executable.clone().unwrap_or_default()).text_size(px(12.))),
                    )
                    .child(
                        button(("external-edit", index), ButtonKind::Secondary, "Editar", false)
                            .on_click(cx.listener(move |this, _, _, cx| this.start(Some(edit_id.clone()), cx))),
                    )
                    .child(
                        button(("external-remove", index), ButtonKind::Icon, "Eliminar", is_running)
                            .tooltip(tooltip(if is_running {
                                "Detené el juego antes de eliminarlo"
                            } else {
                                "Quita el juego y borra su prefijo"
                            }))
                            .when(!is_running, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| this.remove(&id, cx)))
                            }),
                    )
            }))
            .into_any_element()
    }

    fn sources(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_col()
            .child(theme::muted(
                "Con una key de SteamGridDB la búsqueda trae sus portadas, fondos e iconos; sin ella solo busca en Steam.",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .mt_3()
                    .max_w(px(640.))
                    .child(div().flex_1().min_w_0().child(self.api_key.clone()))
                    .child(
                        button("sgdb-save", ButtonKind::Secondary, "Guardar", false)
                            .on_click(cx.listener(|this, _, _, cx| this.save_key(cx))),
                    )
                    .child(
                        button("sgdb-get-key", ButtonKind::Icon, "Conseguir una key", false).on_click(|_, _, _| {
                            if let Err(err) = crate::desktop::open(art::SGDB_KEY_PAGE) {
                                tracing::warn!(%err, "could not open the steamgriddb key page");
                            }
                        }),
                    ),
            )
            .when(!self.key_status.is_empty(), |column| {
                column.child(theme::muted(self.key_status.clone()).mt_2())
            })
    }
}

impl Render for ExternalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editing = self.editing(cx);
        let items = [
            (Tab::Form, if editing { "Editar juego" } else { "Agregar juego" }),
            (Tab::Games, "Tus juegos"),
            (Tab::Sources, "Fuentes de imágenes"),
        ];
        let entity = cx.entity();
        let tabs = crate::widgets::tabs("external-tabs", &items, self.tab, move |tab, _, cx| {
            entity.update(cx, |this, cx| {
                this.tab = tab;
                cx.notify();
            });
        })
        .mt_6()
        .mb_6();

        let body = match (self.tab, &self.form) {
            (_, None) => theme::empty("Elegi un perfil arriba.").into_any_element(),
            (Tab::Form, Some((form, _))) => div()
                .flex()
                .flex_col()
                .when(editing, |column| {
                    column.child(
                        div().flex().mb_3().child(
                            button("external-new", ButtonKind::Secondary, "Agregar otro juego", false)
                                .on_click(cx.listener(|this, _, _, cx| this.start(None, cx))),
                        ),
                    )
                })
                .child(form.clone())
                .into_any_element(),
            (Tab::Games, Some(_)) => self.game_list(cx).into_any_element(),
            (Tab::Sources, Some(_)) => self.sources(cx).into_any_element(),
        };

        theme::page(
            "external",
            div()
                .flex()
                .flex_col()
                .child(theme::h1("Juegos externos"))
                .child(theme::muted(
                    "Juegos de fuera de GOG: cada uno corre en su propio prefijo, igual que un juego instalado, y sale en la Biblioteca con el resto.",
                ))
                .child(tabs)
                .child(body),
        )
    }
}

fn icon(url: Option<&str>) -> impl IntoElement {
    let frame = div()
        .flex_none()
        .size(px(32.))
        .rounded(px(theme::RADIUS_SM))
        .overflow_hidden()
        .bg(rgb(theme::BG_PANEL));
    match url {
        Some(url) => frame.child(
            img(crate::art::source(url, gpui::size(32., 32.)))
                .size_full()
                .object_fit(ObjectFit::Cover),
        ),
        None => frame,
    }
}
