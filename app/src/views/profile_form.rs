//! Creating a profile: the first-run onboarding screen and the topbar's
//! "+ Crear perfil" popup are the same form in two layouts.
//!
//! Onboarding is a gate — decanter shows nothing else until at least one
//! profile exists — and is reached only once in the ordinary case:
//! creating a profile here makes it active, and the check that led here
//! never fires again with a profile already active.

use decanter_core::ProfileKind;
use gpui::{Context, Entity, EventEmitter, Focusable as _, FontWeight, Window, div, prelude::*, px, relative, rgb};

use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Onboarding,
    Popup,
}

/// Fired after a successful create (the new profile is already active by
/// then) and on cancel alike, so the owner of the popup doesn't need to
/// tell the two apart to know it's time to close it.
pub struct Done;

pub struct ProfileForm {
    layout: Layout,
    kind: ProfileKind,
    name: Entity<TextInput>,
    library_path: Entity<TextInput>,
    /// Only set once the user actually picks a folder — leaving the
    /// pre-filled default alone creates the profile with no
    /// `library_path`, same as every profile created before that existed.
    library_path_chosen: bool,
    error: Option<String>,
}

impl EventEmitter<Done> for ProfileForm {}

impl ProfileForm {
    pub fn new(layout: Layout, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new("Nombre del perfil", cx));
        // The pre-filled default — what `Library::default_path()` itself
        // falls back to, rendered as text.
        let default_path =
            decanter_core::Library::default_path().map_or_else(String::new, |path| path.display().to_string());
        let library_path = cx.new(|cx| {
            let mut input = TextInput::new("", cx).read_only();
            input.set_text(default_path, cx);
            input
        });
        cx.subscribe(&name, |this, _, event, cx| match event {
            InputEvent::Changed => cx.notify(),
            InputEvent::Submit => this.submit(cx),
            InputEvent::Cancel if this.layout == Layout::Popup => cx.emit(Done),
            InputEvent::Cancel => {}
        })
        .detach();
        window.focus(&name.focus_handle(cx), cx);
        Self {
            layout,
            kind: ProfileKind::Pc,
            name,
            library_path,
            library_path_chosen: false,
            error: None,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            return;
        }
        // A PS profile never reaches the Proton library, so a folder picked
        // before switching the kind over is dropped rather than stored.
        let chosen_path = (self.library_path_chosen && self.kind == ProfileKind::Pc)
            .then(|| self.library_path.read(cx).text().to_owned());
        match crate::core::create_profile(&name, self.kind, chosen_path.as_deref()) {
            Ok(profile) => {
                tracing::info!(profile_id = %profile.id, name, "profile created");
                let session = AppState::get(cx).session.clone();
                session.update(cx, |session, cx| {
                    session.active_kind = profile.kind;
                    session.active_profile = Some(profile.id);
                    cx.notify();
                });
                cx.emit(Done);
            }
            Err(err) => {
                tracing::error!(name, error = %err, "failed to create profile");
                self.error = Some(err);
                cx.notify();
            }
        }
    }

    fn pick_folder(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title("Elegí dónde vive la biblioteca de este perfil")
                .pick_folder()
                .await
            else {
                return;
            };
            // Content goes under `.../steamapps`, matching Steam's own
            // layout for a library folder — see `Library::root_under`.
            let root = decanter_core::Library::root_under(folder.path());
            tracing::debug!(path = %root.display(), "profile library path picked from file dialog");
            let _ = this.update(cx, |this, cx| {
                this.library_path
                    .update(cx, |input, cx| input.set_text(root.to_string_lossy().into_owned(), cx));
                this.library_path_chosen = true;
                cx.notify();
            });
        })
        .detach();
    }

    fn path_row(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .gap(px(10.))
            .child(div().flex_1().child(self.library_path.clone()))
            .child(
                button("pick-library", ButtonKind::Secondary, "Elegir carpeta", false)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
            )
    }

    fn kind_row(&self, cx: &mut Context<Self>) -> gpui::Div {
        let choice = |id: &'static str, kind: ProfileKind, label: &'static str| {
            let style = if self.kind == kind {
                ButtonKind::Primary
            } else {
                ButtonKind::Secondary
            };
            button(id, style, label, false).on_click(cx.listener(move |this, _, _, cx| {
                this.kind = kind;
                cx.notify();
            }))
        };
        div()
            .flex()
            .gap_2()
            .child(choice("kind-pc", ProfileKind::Pc, "PC"))
            .child(choice("kind-ps", ProfileKind::Ps, "PS"))
    }

    fn error_line(&self) -> Option<gpui::Div> {
        self.error.as_ref().map(|message| theme::error(message.clone()))
    }
}

impl Render for ProfileForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.name.read(cx).text().trim().is_empty();
        match self.layout {
            Layout::Onboarding => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(420.))
                        .max_w(relative(0.8))
                        .flex()
                        .flex_col()
                        .items_center()
                        .text_center()
                        .child(theme::h1("Bienvenido a Decanter"))
                        .child(theme::muted(
                            "Creá un perfil para empezar: PC para tus cuentas de GOG con Proton, PS para \
                             volcados de PS4 y PS5 con los ports nativos.",
                        ))
                        .child(self.kind_row(cx).mt(px(20.)))
                        .child(
                            div()
                                .flex()
                                .gap(px(10.))
                                .mt(px(20.))
                                .w_full()
                                .max_w(px(320.))
                                .child(div().flex_1().child(self.name.clone()))
                                .child(
                                    button("create-profile", ButtonKind::Primary, "Crear perfil", empty)
                                        .when(!empty, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit(cx)))),
                                ),
                        )
                        .when(self.kind == ProfileKind::Pc, |column| {
                            column
                                .child(theme::muted(
                                    "Dónde vive la biblioteca de este perfil (instalaciones, prefijos de Proton):",
                                ))
                                .child(self.path_row(cx).mt(px(20.)).w_full().max_w(px(320.)))
                        })
                        .children(self.error_line()),
                )
                .into_any_element(),
            Layout::Popup => div()
                .w(px(260.))
                .p_4()
                .bg(rgb(theme::BG_PANEL_2))
                .border_1()
                .border_color(rgb(theme::LINE))
                .rounded(px(theme::RADIUS))
                .shadow(theme::popup_shadow())
                .flex()
                .flex_col()
                .child(
                    theme::display(15.)
                        .text_color(rgb(theme::TEXT_MUTED))
                        .mb_3()
                        .child("NUEVO PERFIL"),
                )
                .child(self.kind_row(cx).mb_3())
                .child(self.name.clone())
                .when(self.kind == ProfileKind::Pc, |panel| {
                    panel
                        .child(theme::muted("Dónde vive la biblioteca de este perfil:"))
                        .child(self.path_row(cx).mb_3())
                })
                .children(self.error_line())
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .mt_3()
                        .child(
                            button("cancel-profile", ButtonKind::Secondary, "Cancelar", false)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(Done))),
                        )
                        .child(
                            button("create-profile", ButtonKind::Primary, "Crear", empty)
                                .font_weight(FontWeight::BOLD)
                                .when(!empty, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit(cx)))),
                        ),
                )
                .into_any_element(),
        }
    }
}
