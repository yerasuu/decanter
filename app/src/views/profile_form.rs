//! Creating a profile: the first-run onboarding screen and Ajustes'
//! "+ Añadir persona" are the same form in two layouts.
//!
//! Onboarding is a gate — decanter shows nothing else until at least one
//! profile exists — and is reached only once in the ordinary case:
//! creating a profile here makes it active, and the check that led here
//! never fires again with a profile already active.

use gpui::{Context, Entity, EventEmitter, Focusable as _, FontWeight, Window, div, prelude::*, px, relative, rgb};
use rust_i18n::t;

use crate::i18n::{self, Language};
use crate::state::AppState;
use crate::theme;
use crate::widgets::{ButtonKind, InputEvent, TextInput, button, language_select};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Onboarding,
    Inline,
}

/// Fired after a successful create (the new profile is already active by
/// then) and on cancel alike, so the owner of the inline form doesn't need
/// to tell the two apart to know it's time to close it.
pub struct Done;

pub struct ProfileForm {
    layout: Layout,
    name: Entity<TextInput>,
    library_path: Entity<TextInput>,
    /// Only set once the user actually picks a folder — leaving the
    /// pre-filled default alone creates the profile with no
    /// `library_path`, same as every profile created before that existed.
    library_path_chosen: bool,
    /// The first-run screen's language select, read once instead of on
    /// every render.
    language: Option<Language>,
    /// Picked on the first-run screen, where there may be no database to
    /// keep it in yet: saved again once creating the profile made one.
    language_picked: bool,
    error: Option<String>,
}

impl EventEmitter<Done> for ProfileForm {}

impl ProfileForm {
    pub fn new(layout: Layout, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = Self::name_input(layout, cx);
        // The pre-filled default: the library this profile lands in unless
        // another folder is picked.
        let default_path = decanter_core::bootstrap::active_root()
            .or_else(decanter_core::Library::default_path)
            .map_or_else(String::new, |path| path.display().to_string());
        let library_path = cx.new(|cx| {
            let mut input = TextInput::new("", cx).read_only();
            input.set_text(default_path, cx);
            input
        });
        window.focus(&name.focus_handle(cx), cx);
        Self {
            layout,
            name,
            library_path,
            library_path_chosen: false,
            language: (layout == Layout::Onboarding).then(i18n::saved).flatten(),
            language_picked: false,
            error: None,
        }
    }

    fn name_input(layout: Layout, cx: &mut Context<Self>) -> Entity<TextInput> {
        let name = cx.new(|cx| TextInput::new(t!("chrome.profile_form.name"), cx));
        cx.subscribe(&name, move |this, _, event, cx| match event {
            InputEvent::Changed => cx.notify(),
            InputEvent::Submit => this.submit(cx),
            InputEvent::Cancel if layout == Layout::Inline => cx.emit(Done),
            InputEvent::Cancel => {}
        })
        .detach();
        name
    }

    fn set_language(&mut self, language: Option<Language>, window: &mut Window, cx: &mut Context<Self>) {
        i18n::set(language, cx);
        self.language = language;
        self.language_picked = true;
        // Rebuilt because the placeholder is fixed when the input is made,
        // and it would otherwise stay in the language just left.
        let typed = self.name.read(cx).text().to_owned();
        self.name = Self::name_input(self.layout, cx);
        self.name.update(cx, |input, cx| input.set_text(typed, cx));
        window.focus(&self.name.focus_handle(cx), cx);
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let chosen_path = self
            .library_path_chosen
            .then(|| self.library_path.read(cx).text().to_owned());
        match crate::core::create_profile(&name, chosen_path.as_deref()) {
            Ok(profile) => {
                tracing::info!(profile_id = %profile.id, name, "profile created");
                if self.language_picked {
                    i18n::set(self.language, cx);
                }
                let session = AppState::get(cx).session.clone();
                session.update(cx, |session, cx| {
                    session.reload();
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
                .set_title(t!("chrome.profile_form.pick_title"))
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
                button(
                    "pick-library",
                    ButtonKind::Secondary,
                    t!("chrome.profile_form.pick"),
                    false,
                )
                .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
            )
    }

    fn error_line(&self) -> Option<gpui::Div> {
        self.error.as_ref().map(|message| theme::error(message.clone()))
    }

    /// Labelled in both languages, since whoever reads it may not know the
    /// one the screen started in.
    fn language_picker(&self, cx: &Context<Self>) -> gpui::Div {
        let entity = cx.entity();
        div()
            .absolute()
            .top(px(16.))
            .right(px(16.))
            .flex()
            .items_center()
            .gap_2()
            .child(theme::muted(t!("chrome.onboarding.language")))
            .child(language_select(self.language, move |language, window, cx| {
                entity.update(cx, |this, cx| this.set_language(language, window, cx));
            }))
    }
}

impl Render for ProfileForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.name.read(cx).text().trim().is_empty();
        match self.layout {
            Layout::Onboarding => div()
                .size_full()
                .relative()
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
                        .child(theme::h1(t!("chrome.onboarding.title")))
                        .child(theme::muted(t!("chrome.onboarding.intro")))
                        .child(
                            div()
                                .flex()
                                .gap(px(10.))
                                .mt(px(20.))
                                .w_full()
                                .max_w(px(320.))
                                .child(div().flex_1().child(self.name.clone()))
                                .child(
                                    button(
                                        "create-profile",
                                        ButtonKind::Primary,
                                        t!("chrome.profiles.create"),
                                        empty,
                                    )
                                    .when(!empty, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit(cx)))),
                                ),
                        )
                        .child(theme::muted(if cfg!(windows) {
                            t!("chrome.onboarding.library_native")
                        } else {
                            t!("chrome.onboarding.library")
                        }))
                        .child(self.path_row(cx).mt(px(20.)).w_full().max_w(px(320.)))
                        .children(self.error_line()),
                )
                .child(self.language_picker(cx))
                .into_any_element(),
            Layout::Inline => div()
                .max_w(px(520.))
                .p_4()
                .bg(rgb(theme::BG_PANEL_2))
                .border_1()
                .border_color(rgb(theme::LINE))
                .rounded(px(theme::RADIUS))
                .flex()
                .flex_col()
                .child(
                    theme::display(15.)
                        .text_color(rgb(theme::TEXT_MUTED))
                        .mb_3()
                        .child(t!("chrome.profile_form.title")),
                )
                .child(self.name.clone())
                .child(theme::muted(t!("chrome.profile_form.library")))
                .child(self.path_row(cx).mb_3())
                .children(self.error_line())
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .mt_3()
                        .child(
                            button("cancel-profile", ButtonKind::Secondary, t!("common.cancel"), false)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(Done))),
                        )
                        .child(
                            button(
                                "create-profile",
                                ButtonKind::Primary,
                                t!("chrome.profile_form.create"),
                                empty,
                            )
                            .font_weight(FontWeight::BOLD)
                            .when(!empty, |b| b.on_click(cx.listener(|this, _, _, cx| this.submit(cx)))),
                        ),
                )
                .into_any_element(),
        }
    }
}
