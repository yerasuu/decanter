//! Adding or editing one external game: its name, what launches it —
//! optionally installed first by running its setup program in the game's
//! own prefix, the way Heroic does — and its art.

mod art;
mod prefix;
mod umu;

use decanter_core::{GameArt, InstalledGame};
use gpui::{Context, Entity, EventEmitter, ScrollHandle, Task, Window, div, prelude::*, px};
use rust_i18n::t;

use crate::external::art::Candidates;
use crate::external::import::Mode as ImportMode;
use crate::theme;
use crate::widgets::{ButtonKind, TextInput, button};

pub enum FormEvent {
    /// Written to disk; the list and the library should re-read.
    Saved,
}

enum Installer {
    Idle,
    Running,
    /// Ran in this game's prefix: the executable picker starts in its `C:`.
    Done,
    Failed(String),
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
    /// An edit keeps its Proton pin for the installer too.
    proton: Option<String>,
    umu: Option<crate::external::UmuEntry>,
    /// Kept through an edit: whose prefix this game runs in.
    prefix_of: Option<String>,
    /// That game's name, to say so on the form.
    host_name: Option<String>,
    /// The library folders a new game can go in, with their free space.
    folders: Vec<(std::path::PathBuf, Option<u64>)>,
    /// The folder picked for a new game; `None` is the main one.
    library_root: Option<std::path::PathBuf>,
    name: Entity<TextInput>,
    executable: Entity<TextInput>,
    art: GameArt,
    installer: Installer,
    /// Another tool's prefix to move in on save, picked by folder.
    import_from: Option<std::path::PathBuf>,
    import_mode: ImportMode,
    /// Set while a save is bringing a prefix in.
    importing: bool,
    progress: std::sync::Arc<crate::external::import::Progress>,
    /// Redraws the progress bar while `importing`: the copy runs on a
    /// blocking thread that can't notify this view itself.
    _ticker: Option<Task<()>>,
    search: Search,
    /// The UMU database rows the name matched, once looked up.
    umu_matches: Option<Result<Vec<crate::external::umu::Match>, String>>,
    umu_searching: bool,
    _umu_work: Option<Task<()>>,
    error: String,
    _work: Option<Task<()>>,
    /// One per [`KINDS`](art::KINDS) row: the candidates scroll sideways and are paged
    /// by the arrows, which need the row's offset and width.
    scrolls: [ScrollHandle; 3],
    paging: Option<Task<()>>,
}

impl EventEmitter<FormEvent> for GameForm {}

impl Drop for GameForm {
    /// Leaving the form ends any chance of retrying a failed copy into its
    /// staging directory, so that goes. One still running is left to
    /// finish: it adds the game on its own.
    fn drop(&mut self) {
        if !self.importing && self.import_from.is_some() {
            crate::external::import::abandon(&self.profile_id, &self.id);
        }
    }
}

impl GameForm {
    pub fn new(profile_id: String, editing: Option<InstalledGame>, cx: &mut Context<Self>) -> Self {
        let folders = library_folders(&profile_id);
        let name = cx.new(|cx| TextInput::new(t!("external.form.name"), cx));
        let executable = cx.new(|cx| TextInput::new(t!("external.form.executable"), cx));
        let Some(game) = editing else {
            return Self {
                profile_id,
                id: uuid::Uuid::new_v4().to_string(),
                editing: false,
                proton: None,
                umu: None,
                prefix_of: None,
                host_name: None,
                folders,
                library_root: None,
                name,
                executable,
                art: GameArt::default(),
                installer: Installer::Idle,
                import_from: None,
                import_mode: ImportMode::Copy,
                importing: false,
                progress: Default::default(),
                _ticker: None,
                search: Search::Idle,
                umu_matches: None,
                umu_searching: false,
                _umu_work: None,
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
            profile_id,
            id: game.game_id,
            editing: true,
            proton: game.proton,
            prefix_of: game.prefix_of,
            host_name: None,
            folders,
            library_root: None,
            umu: game.umu_id.map(|id| crate::external::UmuEntry {
                id,
                store: game.umu_store,
            }),
            name,
            executable,
            art: game.art,
            installer: Installer::Idle,
            import_from: None,
            import_mode: ImportMode::Copy,
            importing: false,
            progress: Default::default(),
            _ticker: None,
            search: Search::Idle,
            umu_matches: None,
            umu_searching: false,
            _umu_work: None,
            error: String::new(),
            _work: None,
            scrolls: Default::default(),
            paging: None,
        }
    }

    /// A new game a launcher installed in `host`'s prefix: filled in from
    /// what was found, running in that prefix with `host`'s Proton, and its
    /// art and UMU entry already being looked up by name.
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
        this.proton.clone_from(&host.proton);
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
    fn pick_executable(&mut self, cx: &mut Context<Self>) {
        let start = match (&self.import_from, &self.installer) {
            (Some(picked), _) => crate::external::import::wine_root(picked).map(|root| root.join("drive_c")),
            (None, Installer::Done) => {
                crate::external::drive_c(&self.profile_id, &self.id, self.library_root.as_deref())
            }
            (None, _) => None,
        };
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
        if let Some(picked) = self.import_from.clone() {
            self.importing = true;
            self.progress = std::sync::Arc::default();
            let (profile_id, id, mode) = (self.profile_id.clone(), self.id.clone(), self.import_mode);
            let progress = std::sync::Arc::clone(&self.progress);
            self._ticker = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(250))
                        .await;
                    let still = this.update(cx, |this, cx| {
                        cx.notify();
                        this.importing
                    });
                    if !matches!(still, Ok(true)) {
                        return;
                    }
                }
            }));
            self._work = Some(cx.spawn(async move |this, cx| {
                let result = tokio::task::spawn_blocking(move || {
                    crate::external::import::import(&profile_id, &id, &picked, mode, &progress, draft)
                })
                .await
                .unwrap_or_else(|err| Err(err.to_string()));
                let _ = this.update(cx, |this, cx| {
                    this.importing = false;
                    match result {
                        Ok(()) => cx.emit(FormEvent::Saved),
                        Err(err) => this.error = err,
                    }
                    cx.notify();
                });
            }));
            return;
        }
        match crate::external::save(&self.profile_id, &self.id, draft) {
            Ok(()) => cx.emit(FormEvent::Saved),
            Err(err) => self.error = err,
        }
    }
}

impl Render for GameForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = matches!(self.installer, Installer::Running) || self.importing;
        let save_label = match (
            self.editing,
            self.importing,
            self.import_from.is_some(),
            self.import_mode,
        ) {
            (true, ..) => t!("external.form.save.edit"),
            (false, true, _, ImportMode::Copy) => t!("external.form.save.copying"),
            (false, true, _, ImportMode::Move) => t!("external.form.save.moving"),
            (false, false, true, ImportMode::Copy) => t!("external.form.save.copy"),
            (false, false, true, ImportMode::Move) => t!("external.form.save.move"),
            (false, false, false, _) => t!("external.form.save.add"),
        };
        div()
            .flex()
            .flex_col()
            .child(div().mb_3().max_w(px(520.)).child(self.name.clone()))
            .children(self.folder_row(cx))
            .when(!self.editing && self.prefix_of.is_none(), |form| {
                form.child(self.installer_row(cx))
            })
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
            .child(self.umu_section(cx))
            .when(self.importing, |form| form.child(self.import_progress()))
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

/// `profile_id`'s library folders, main first, with each one's free space.
fn library_folders(profile_id: &str) -> Vec<(std::path::PathBuf, Option<u64>)> {
    crate::core::library_for_profile(profile_id)
        .map(|library| library.roots())
        .unwrap_or_default()
        .into_iter()
        .map(|root| {
            let free = decanter_core::disk_usage(&root).map(|(free, _)| free);
            (root, free)
        })
        .collect()
}
