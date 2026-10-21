//! Adding or editing one external game: its name, what launches it —
//! optionally installed first by running its setup program (in the game's
//! own prefix under Proton), the way Heroic does — and its art.

mod art;
mod installer;
#[cfg(proton)]
mod prefix;
#[cfg(proton)]
mod umu;

use decanter_core::{GameArt, InstalledGame};
use gpui::{Context, Div, Entity, EventEmitter, ScrollHandle, Task, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::external::art::Candidates;
use crate::i18n::Text;
use crate::theme;
use crate::widgets::{ButtonKind, TextInput, button};

pub enum FormEvent {
    /// Written to disk; the list and the library should re-read.
    Saved,
}

enum Search {
    Idle,
    Searching,
    Done(Candidates),
}

pub struct GameForm {
    profile_id: String,
    /// The game's id from the start, not only once saved: an installer
    /// run before saving has to land in the prefix the game will use.
    id: String,
    editing: bool,
    umu: Option<crate::external::UmuEntry>,
    /// Kept through an edit: whose prefix this game runs in.
    prefix_of: Option<String>,
    /// That game's name, to say so on the form.
    host_name: Option<String>,
    /// The folder picked for a new game; `None` is the main one.
    library_root: Option<std::path::PathBuf>,
    name: Entity<TextInput>,
    executable: Entity<TextInput>,
    art: GameArt,
    search: Search,
    error: Option<Text>,
    _work: Option<Task<()>>,
    /// One per [`KINDS`](art::KINDS) row: the candidates scroll sideways and are paged
    /// by the arrows, which need the row's offset and width.
    scrolls: [ScrollHandle; 3],
    paging: Option<Task<()>>,
    installer: installer::Installer,
    /// The prefix import and UMU entry, which only a game run under Proton
    /// has.
    #[cfg(proton)]
    wine: prefix::Wine,
}

/// The optional rows around the form's fields, in the order they show.
#[derive(Default)]
struct Rows {
    folder: Option<Div>,
    installer: Option<Div>,
    umu: Option<Div>,
    progress: Option<Div>,
}

impl EventEmitter<FormEvent> for GameForm {}

impl GameForm {
    pub fn new(profile_id: String, editing: Option<InstalledGame>, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new(Text::new("external.form.name"), cx));
        let executable = cx.new(|cx| TextInput::new(Text::new("external.form.executable"), cx));
        let Some(game) = editing else {
            return Self {
                #[cfg(proton)]
                wine: prefix::Wine::new(&profile_id, None),
                profile_id,
                id: uuid::Uuid::new_v4().to_string(),
                editing: false,
                umu: None,
                prefix_of: None,
                host_name: None,
                library_root: None,
                name,
                executable,
                art: GameArt::default(),
                search: Search::Idle,
                error: None,
                _work: None,
                scrolls: Default::default(),
                paging: None,
                installer: installer::Installer::Idle,
            };
        };
        name.update(cx, |input, cx| input.set_text(game.name.clone(), cx));
        if let Some(path) = crate::core::executable_for(&game) {
            executable.update(cx, |input, cx| input.set_text(path.to_string_lossy().into_owned(), cx));
        }
        Self {
            #[cfg(proton)]
            wine: prefix::Wine::new(&profile_id, game.proton),
            profile_id,
            id: game.game_id,
            editing: true,
            prefix_of: game.prefix_of,
            host_name: None,
            library_root: None,
            umu: game.umu_id.map(|id| crate::external::UmuEntry {
                id,
                store: game.umu_store,
            }),
            name,
            executable,
            art: game.art,
            search: Search::Idle,
            error: None,
            _work: None,
            scrolls: Default::default(),
            paging: None,
            installer: installer::Installer::Idle,
        }
    }

    /// A new game a launcher installed in `host`'s prefix: filled in from
    /// what was found, running in that prefix with `host`'s Proton, and its
    /// art and UMU entry already being looked up by name.
    #[cfg(proton)]
    pub fn hosted(
        profile_id: String,
        host: &InstalledGame,
        found: &crate::external::launchers::Detected,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(profile_id, None, cx);
        this.prefix_of = Some(host.game_id.clone());
        // Kept beside its launcher, so the two stay on one disk.
        this.library_root = crate::core::library_for_profile(&this.profile_id)
            .ok()
            .and_then(|library| library.root_of(&this.profile_id, &host.game_id));
        this.host_name = Some(host.name.clone());
        this.wine.proton.clone_from(&host.proton);
        this.name.update(cx, |input, cx| input.set_text(found.name.clone(), cx));
        this.executable.update(cx, |input, cx| {
            input.set_text(found.executable.to_string_lossy().into_owned(), cx);
        });
        this.search_art(cx);
        this
    }

    pub fn is_editing(&self, id: &str) -> bool {
        self.editing && self.id == id
    }

    /// Native file picker (XDG portal) rather than a typed path: typos in
    /// an absolute path otherwise only show up as a launch that does
    /// nothing. After an installer ran it opens where that installer most
    /// likely put the game.
    fn pick_executable(&mut self, cx: &mut Context<Self>) {
        let start = self.executable_start();
        cx.spawn(async move |this, cx| {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title(t!("external.form.pick_exe.title"))
                .add_filter(t!("external.form.pick_exe.filter"), &["exe", "bat"]);
            if let Some(start) = start {
                dialog = dialog.set_directory(start);
            }
            let Some(file) = dialog.pick_file().await else {
                return;
            };
            let chosen = file.path().to_path_buf();
            tracing::debug!(path = %chosen.display(), "external game executable picked");
            let _ = this.update(cx, |this, cx| {
                if this.name.read(cx).text().trim().is_empty()
                    && let Some(stem) = chosen.file_stem().and_then(|stem| stem.to_str())
                {
                    this.name.update(cx, |input, cx| input.set_text(stem.to_owned(), cx));
                }
                this.executable.update(cx, |input, cx| {
                    input.set_text(chosen.to_string_lossy().into_owned(), cx)
                });
            });
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        let executable = std::path::PathBuf::from(self.executable.read(cx).text().trim());
        cx.notify();
        if name.is_empty() || executable.as_os_str().is_empty() {
            self.error = Some(Text::new("external.form.errors.missing"));
            return;
        }
        if !executable.is_file() {
            self.error = Some(Text::new("external.form.errors.no_executable"));
            return;
        }
        let draft = crate::external::Draft {
            name,
            executable,
            art: self.art.clone(),
            umu: self.umu.clone(),
            prefix_of: self.prefix_of.clone(),
            library_root: self.library_root.clone(),
        };
        #[cfg(proton)]
        let Some(draft) = self.import_on_save(draft, cx) else {
            return;
        };
        match crate::external::save(&self.profile_id, &self.id, draft) {
            Ok(()) => cx.emit(FormEvent::Saved),
            Err(err) => self.error = Some(err),
        }
    }
}

impl Render for GameForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(proton)]
        let (busy, save_label, rows) = (self.busy(), self.save_label(), self.wine_rows(cx));
        #[cfg(not(proton))]
        let (busy, save_label, rows) = (
            matches!(self.installer, installer::Installer::Running),
            if self.editing {
                t!("external.form.save.edit")
            } else {
                t!("external.form.save.add")
            },
            Rows {
                installer: (!self.editing).then(|| self.installer_row(cx)),
                ..Rows::default()
            },
        );
        div()
            .flex()
            .flex_col()
            .child(div().mb_3().max_w(px(520.)).child(self.name.clone()))
            .children(rows.folder)
            .children(rows.installer)
            .children(
                self.host_name
                    .as_ref()
                    .map(|host| theme::muted(t!("external.form.hosted", host = host)).mb_3()),
            )
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .mb_3()
                    .max_w(px(640.))
                    .child(div().flex_1().min_w_0().child(self.executable.clone()))
                    .child(
                        button(
                            "external-pick-exe",
                            ButtonKind::Secondary,
                            t!("external.form.pick_exe.button"),
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.pick_executable(cx))),
                    ),
            )
            .child(self.art_section(cx))
            .children(rows.umu)
            .children(rows.progress)
            .when_some(self.error.as_ref(), |form, error| {
                form.child(div().mt_3().child(theme::error(error)))
            })
            .child(
                div().flex().mt_4().child(
                    button("external-save", ButtonKind::Primary, save_label, busy)
                        .when(!busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.save(cx)))),
                ),
            )
    }
}
