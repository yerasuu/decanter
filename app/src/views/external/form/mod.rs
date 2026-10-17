//! Adding or editing one external game: its name, what launches it —
//! optionally installed first by running its setup program in the game's
//! own prefix, the way Heroic does — and its art.

mod art;
#[cfg(not(windows))]
mod prefix;
#[cfg(not(windows))]
mod umu;

use decanter_core::{GameArt, InstalledGame};
use gpui::{Context, Div, Entity, EventEmitter, ScrollHandle, Task, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::external::art::Candidates;
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
    error: String,
    _work: Option<Task<()>>,
    /// One per [`KINDS`](art::KINDS) row: the candidates scroll sideways and are paged
    /// by the arrows, which need the row's offset and width.
    scrolls: [ScrollHandle; 3],
    paging: Option<Task<()>>,
    /// The installer, prefix import and UMU entry, which only a game run
    /// under Proton has.
    #[cfg(not(windows))]
    wine: prefix::Wine,
}

/// The rows only a game run under Proton has, in the order they show.
#[derive(Default)]
struct WineRows {
    folder: Option<Div>,
    installer: Option<Div>,
    umu: Option<Div>,
    progress: Option<Div>,
}

impl EventEmitter<FormEvent> for GameForm {}

impl GameForm {
    pub fn new(profile_id: String, editing: Option<InstalledGame>, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| TextInput::new(t!("external.form.name"), cx));
        let executable = cx.new(|cx| TextInput::new(t!("external.form.executable"), cx));
        let Some(game) = editing else {
            return Self {
                #[cfg(not(windows))]
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
                error: String::new(),
                _work: None,
                scrolls: Default::default(),
                paging: None,
            };
        };
        name.update(cx, |input, cx| input.set_text(game.name.clone(), cx));
        if let Some(path) = crate::core::executable_for(&game) {
            executable.update(cx, |input, cx| input.set_text(path.to_string_lossy().into_owned(), cx));
        }
        Self {
            #[cfg(not(windows))]
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
            error: String::new(),
            _work: None,
            scrolls: Default::default(),
            paging: None,
        }
    }

    /// A new game a launcher installed in `host`'s prefix: filled in from
    /// what was found, running in that prefix with `host`'s Proton, and its
    /// art and UMU entry already being looked up by name.
    #[cfg(not(windows))]
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
    /// nothing. After an installer ran it opens in the prefix's `C:`,
    /// where that installer put the game.
    #[cfg_attr(
        windows,
        expect(clippy::unused_self, reason = "only a prefix gives the picker a folder to open in")
    )]
    fn pick_executable(&mut self, cx: &mut Context<Self>) {
        #[cfg(not(windows))]
        let start = self.executable_start();
        #[cfg(windows)]
        let start: Option<std::path::PathBuf> = None;
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
            self.error = t!("external.form.errors.missing").into_owned();
            return;
        }
        if !executable.is_file() {
            self.error = t!("external.form.errors.no_executable").into_owned();
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
        #[cfg(not(windows))]
        let Some(draft) = self.import_on_save(draft, cx) else {
            return;
        };
        match crate::external::save(&self.profile_id, &self.id, draft) {
            Ok(()) => cx.emit(FormEvent::Saved),
            Err(err) => self.error = err,
        }
    }
}

impl Render for GameForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(not(windows))]
        let (busy, save_label, rows) = (self.busy(), self.save_label(), self.wine_rows(cx));
        #[cfg(windows)]
        let (busy, save_label, rows) = (
            false,
            if self.editing {
                t!("external.form.save.edit")
            } else {
                t!("external.form.save.add")
            },
            WineRows::default(),
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
            .when(!self.error.is_empty(), |form| {
                form.child(div().mt_3().child(theme::error(self.error.clone())))
            })
            .child(
                div().flex().mt_4().child(
                    button("external-save", ButtonKind::Primary, save_label, busy)
                        .when(!busy, |b| b.on_click(cx.listener(|this, _, _, cx| this.save(cx)))),
                ),
            )
    }
}
