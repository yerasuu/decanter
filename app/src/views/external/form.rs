//! Adding or editing one external game: its name, what launches it —
//! optionally installed first by running its setup program in the game's
//! own prefix, the way Heroic does — and its art.

use decanter_core::{GameArt, InstalledGame};
use gpui::{
    AnyElement, Context, Div, Entity, EventEmitter, FontWeight, MouseButton, ObjectFit, ScrollHandle, Task, Window,
    div, img, linear_color_stop, linear_gradient, point, prelude::*, px, relative, rgb,
};

use crate::external::art::{Candidate, Candidates, Source};
use crate::external::import::Mode as ImportMode;
use crate::theme;
use crate::widgets::{ButtonKind, TextInput, button, tooltip};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArtKind {
    Cover,
    Hero,
    Icon,
}

impl ArtKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Cover => "Portada",
            Self::Hero => "Fondo",
            Self::Icon => "Icono",
        }
    }

    /// How a thumbnail is drawn: the shape the art has in the library.
    const fn thumb(self) -> (f32, f32) {
        match self {
            Self::Cover => (80., 120.),
            Self::Hero => (213., 69.),
            Self::Icon => (56., 56.),
        }
    }

    fn slot(self, art: &mut GameArt) -> &mut Option<String> {
        match self {
            Self::Cover => &mut art.cover,
            Self::Hero => &mut art.hero,
            Self::Icon => &mut art.icon,
        }
    }

    fn candidates(self, found: &Candidates) -> &[Candidate] {
        match self {
            Self::Cover => &found.covers,
            Self::Hero => &found.heroes,
            Self::Icon => &found.icons,
        }
    }
}

const KINDS: [ArtKind; 3] = [ArtKind::Cover, ArtKind::Hero, ArtKind::Icon];

pub struct GameForm {
    profile_id: String,
    /// The game's id from the start, not only once saved: an installer
    /// run before saving has to land in the prefix the game will use.
    id: String,
    editing: bool,
    /// An edit keeps its Proton pin for the installer too.
    proton: Option<String>,
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
    error: String,
    _work: Option<Task<()>>,
    /// One per [`KINDS`] row: the candidates scroll sideways and are paged
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
        let name = cx.new(|cx| TextInput::new("Nombre", cx));
        let executable = cx.new(|cx| TextInput::new("Ejecutable (.exe)", cx));
        let Some(game) = editing else {
            return Self {
                profile_id,
                id: uuid::Uuid::new_v4().to_string(),
                editing: false,
                proton: None,
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
            error: String::new(),
            _work: None,
            scrolls: Default::default(),
            paging: None,
        }
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
            (None, Installer::Done) => crate::external::drive_c(&self.profile_id, &self.id),
            (None, _) => None,
        };
        cx.spawn(async move |this, cx| {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title("Elegí el ejecutable del juego")
                .add_filter("Ejecutables de Windows", &["exe", "bat"]);
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

    fn run_installer(&mut self, cx: &mut Context<Self>) {
        let (profile_id, id, proton) = (self.profile_id.clone(), self.id.clone(), self.proton.clone());
        self._work = Some(cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Elegí el instalador")
                .add_filter("Instaladores de Windows", &["exe"])
                .pick_file()
                .await
            else {
                return;
            };
            let installer = file.path().to_path_buf();
            let _ = this.update(cx, |this, cx| {
                this.installer = Installer::Running;
                this.error.clear();
                cx.notify();
            });
            let result = crate::external::run_installer(&profile_id, &id, &installer, proton).await;
            let _ = this.update(cx, |this, cx| {
                this.installer = match result {
                    Ok(()) => Installer::Done,
                    Err(err) => Installer::Failed(err),
                };
                cx.notify();
                if matches!(this.installer, Installer::Done) {
                    this.pick_executable(cx);
                }
            });
        }));
    }

    /// Fills whichever kinds are still empty with the best match, so a
    /// search alone is enough for a game that's easy to find.
    fn search_art(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        if name.is_empty() {
            self.error = "Poné un nombre para buscar sus imágenes.".to_owned();
            cx.notify();
            return;
        }
        self.search = Search::Searching;
        self.error.clear();
        cx.notify();
        self._work = Some(cx.spawn(async move |this, cx| {
            let found = crate::external::art::search(&name).await;
            let _ = this.update(cx, |this, cx| {
                for kind in KINDS {
                    let best = kind.candidates(&found).first().map(|candidate| candidate.url.clone());
                    let slot = kind.slot(&mut this.art);
                    if slot.is_none() {
                        *slot = best;
                    }
                }
                this.search = Search::Done(found);
                cx.notify();
            });
        }));
    }

    fn pick_image(&mut self, kind: ArtKind, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Elegí una imagen")
                .add_filter("Imágenes", &["png", "jpg", "jpeg", "webp", "ico"])
                .pick_file()
                .await
            else {
                return;
            };
            let url = format!("file://{}", file.path().display());
            let _ = this.update(cx, |this, cx| {
                *kind.slot(&mut this.art) = Some(url);
                cx.notify();
            });
        })
        .detach();
    }

    /// Picks another tool's prefix to bring in, then the game inside it.
    /// Nothing moves until the game is saved.
    fn pick_prefix(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title("Elegí la carpeta del prefijo")
                .pick_folder()
                .await
            else {
                return;
            };
            let picked = folder.path().to_path_buf();
            let _ = this.update(cx, |this, cx| {
                if crate::external::import::wine_root(&picked).is_none() {
                    this.error = "Esa carpeta no es un prefijo de Wine (no tiene drive_c).".to_owned();
                    cx.notify();
                    return;
                }
                tracing::debug!(prefix = %picked.display(), "prefix picked for import");
                this.error.clear();
                this.import_from = Some(picked);
                this.pick_executable(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).text().trim().to_owned();
        let executable = std::path::PathBuf::from(self.executable.read(cx).text().trim());
        cx.notify();
        if name.is_empty() || executable.as_os_str().is_empty() {
            self.error = "Poné un nombre y un ejecutable.".to_owned();
            return;
        }
        if !executable.is_file() {
            self.error = "Ese ejecutable no existe.".to_owned();
            return;
        }
        let draft = crate::external::Draft {
            name,
            executable,
            art: self.art.clone(),
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

    fn installer_row(&self, cx: &Context<Self>) -> Div {
        let running = matches!(self.installer, Installer::Running);
        // One or the other: an imported prefix replaces the game's own,
        // which an installer has already written into.
        let installed = matches!(self.installer, Installer::Done);
        let status = match (&self.import_from, &self.installer) {
            (Some(picked), _) => theme::muted(match self.import_mode {
                ImportMode::Copy => format!(
                    "Al agregar, el prefijo {} se copia a decanter: la otra herramienta lo sigue teniendo, pero ocupa el doble y las partidas quedan separadas.",
                    picked.display()
                ),
                ImportMode::Move => format!(
                    "Al agregar, el prefijo {} se mueve a decanter: no ocupa más espacio, pero la otra herramienta se queda sin él.",
                    picked.display()
                ),
            }),
            (None, Installer::Idle) => theme::muted(
                "¿Viene con instalador? Corrélo acá. ¿Ya está instalado en el prefijo de otra herramienta (Lutris, Bottles, Heroic, Steam)? Importalo.",
            ),
            (None, Installer::Running) => theme::muted("Instalando... seguí los pasos del instalador."),
            (None, Installer::Done) => theme::muted("Instalador terminado. Elegí el ejecutable que dejó instalado."),
            (None, Installer::Failed(err)) => theme::error(err.clone()),
        };
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .mb_3()
            .child(
                button(
                    "external-run-installer",
                    ButtonKind::Secondary,
                    "Ejecutar instalador...",
                    running,
                )
                .when(!running, |b| {
                    b.on_click(cx.listener(|this, _, _, cx| this.run_installer(cx)))
                }),
            )
            .child(match &self.import_from {
                Some(_) => div()
                    .flex()
                    .gap(px(6.))
                    .child(self.mode_button(ImportMode::Copy, "Copiar", cx))
                    .child(self.mode_button(ImportMode::Move, "Mover", cx))
                    .child(
                        button("external-import-cancel", ButtonKind::Icon, "No importar", false).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.import_from = None;
                                cx.notify();
                            },
                        )),
                    ),
                None => div().child(
                    button(
                        "external-import-prefix",
                        ButtonKind::Secondary,
                        "Importar prefijo...",
                        running || installed,
                    )
                    .when(!running && !installed, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.pick_prefix(cx)))
                    }),
                ),
            })
            .child(div().flex_1().min_w_0().child(status))
    }

    fn import_progress(&self) -> Div {
        let snapshot = self.progress.snapshot();
        let text = match snapshot {
            Some((copied, total)) => format!(
                "{} de {} ({:.0}%)",
                crate::format::bytes(copied),
                crate::format::bytes(total),
                copied as f64 * 100. / total as f64
            ),
            None => "Midiendo el prefijo...".to_owned(),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .mt_4()
            .max_w(px(520.))
            .child(crate::widgets::progress_bar(
                "external-import-progress",
                snapshot.map(|(copied, total)| copied as f64 / total as f64),
                6.,
            ))
            .child(theme::muted(text).text_size(px(12.)))
    }

    fn mode_button(&self, mode: ImportMode, label: &'static str, cx: &Context<Self>) -> impl IntoElement + use<> {
        let kind = if self.import_mode == mode {
            ButtonKind::Primary
        } else {
            ButtonKind::Secondary
        };
        button(("external-import-mode", mode as usize), kind, label, false).on_click(cx.listener(
            move |this, _, _, cx| {
                this.import_mode = mode;
                cx.notify();
            },
        ))
    }

    fn art_section(&self, cx: &Context<Self>) -> Div {
        let searching = matches!(self.search, Search::Searching);
        let found = match &self.search {
            Search::Done(found) => Some(found),
            Search::Idle | Search::Searching => None,
        };
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .mb_3()
                    .child(
                        button(
                            "external-search-art",
                            ButtonKind::Secondary,
                            if searching { "Buscando..." } else { "Buscar imágenes" },
                            searching,
                        )
                        .when(!searching, |b| {
                            b.on_click(cx.listener(|this, _, _, cx| this.search_art(cx)))
                        }),
                    )
                    .child(theme::muted("Por nombre, en SteamGridDB y en Steam.")),
            )
            .children(found.map(|found| {
                div()
                    .flex()
                    .flex_col()
                    .when(found.is_empty(), |notes| {
                        notes.child(theme::muted("No se encontraron imágenes con ese nombre."))
                    })
                    .children(found.problems.iter().map(|problem| {
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(problem.clone())
                    }))
            }))
            .children(KINDS.map(|kind| self.art_row(kind, found, cx)))
    }

    /// One kind's current pick, then every candidate for it: click one to
    /// use it.
    fn art_row(&self, kind: ArtKind, found: Option<&Candidates>, cx: &Context<Self>) -> Div {
        let chosen = self.art_ref(kind).clone();
        let candidates = found.map(|found| kind.candidates(found)).unwrap_or_default();
        let thumbs = candidates.iter().enumerate().map(|(index, candidate)| {
            let selected = chosen.as_deref() == Some(candidate.url.as_str());
            let url = candidate.url.clone();
            thumb(kind, &candidate.url, selected)
                .id(gpui::ElementId::Name(format!("art-{}-{index}", kind.label()).into()))
                .cursor_pointer()
                .tooltip(tooltip(match candidate.source {
                    Source::SteamGridDb => "SteamGridDB",
                    Source::Steam => "Steam",
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    *kind.slot(&mut this.art) = Some(url.clone());
                    cx.notify();
                }))
                .into_any_element()
        });
        let current: AnyElement = match &chosen {
            Some(url) => thumb(kind, url, true).into_any_element(),
            None => empty_thumb(kind).into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .mt_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .mb_2()
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(kind.label()),
                    )
                    .when(!candidates.is_empty(), |row| {
                        row.child(theme::muted(format!("{} encontradas", candidates.len())).text_size(px(12.)))
                    })
                    .child(
                        button(("art-file", kind as usize), ButtonKind::Icon, "Archivo...", false)
                            .on_click(cx.listener(move |this, _, _, cx| this.pick_image(kind, cx))),
                    )
                    .when(chosen.is_some(), |row| {
                        row.child(
                            button(("art-clear", kind as usize), ButtonKind::Icon, "Quitar", false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    *kind.slot(&mut this.art) = None;
                                    cx.notify();
                                }),
                            ),
                        )
                    }),
            )
            .child(div().flex().gap(px(8.)).child(div().flex_none().child(current)).when(
                !candidates.is_empty(),
                |row| {
                    row.child(div().flex_none().w(px(1.)).bg(rgb(theme::LINE)))
                        .child(self.strip(kind, thumbs, cx))
                },
            ))
    }

    /// The candidates, in a row that scrolls sideways — by the wheel or a
    /// trackpad, or paged by the edge arrows, which only show while there
    /// is more that way.
    fn strip(&self, kind: ArtKind, thumbs: impl Iterator<Item = AnyElement>, cx: &Context<Self>) -> impl IntoElement {
        let scroll = &self.scrolls[kind as usize];
        // A pixel of slack either way: an eased scroll often lands a hair
        // off the exact end. A row not laid out yet reports no overflow,
        // so neither arrow shows until it has been measured.
        let offset = scroll.offset().x;
        let at_start = offset >= px(-1.);
        let at_end = -offset >= scroll.max_offset().x - px(1.);
        let arrow = |left: bool| {
            let (id, label, glyph, direction) = if left {
                ("art-arrow-left", "Ver anteriores", "‹", -1.)
            } else {
                ("art-arrow-right", "Ver siguientes", "›", 1.)
            };
            div()
                .id((id, kind as usize))
                .absolute()
                .top_0()
                .bottom(px(8.))
                .w(px(40.))
                .when(left, |arrow| arrow.left_0())
                .when(!left, |arrow| arrow.right_0())
                .flex()
                .items_center()
                .justify_center()
                .bg(linear_gradient(
                    if left { 90. } else { 270. },
                    linear_color_stop(rgb(theme::BG_DEEP), 0.4),
                    linear_color_stop(theme::alpha(theme::BG_DEEP, 0.), 1.),
                ))
                .text_size(px(22.))
                .line_height(relative(1.))
                .text_color(rgb(theme::TEXT_BRIGHT))
                .cursor_pointer()
                .hover(|arrow| arrow.text_color(rgb(theme::ACCENT_BLUE)))
                // Otherwise the thumbnail under the arrow takes the click too.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .tooltip(tooltip(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.page(kind, direction, cx);
                }))
                .child(glyph)
        };
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .id(("art-candidates", kind as usize))
                    .flex()
                    .gap(px(8.))
                    .pb_2()
                    .overflow_x_scroll()
                    // A vertical wheel over the row scrolls the page past it
                    // instead of the row sideways.
                    .restrict_scroll_to_axis()
                    .track_scroll(scroll)
                    // The arrows depend on the offset, so a scroll has to
                    // re-render, not just repaint.
                    .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
                    .children(thumbs),
            )
            .when(!at_start, |strip| strip.child(arrow(true)))
            .when(!at_end, |strip| strip.child(arrow(false)))
    }

    /// Scrolls a row by most of its visible width, eased, the same way the
    /// library's shelves page — a click always brings in a fresh set.
    fn page(&mut self, kind: ArtKind, direction: f32, cx: &mut Context<Self>) {
        let handle = self.scrolls[kind as usize].clone();
        let start = handle.offset();
        let width = handle.bounds().size.width;
        // GPUI's offset grows negative as the row scrolls right.
        let target = (start.x - width * (0.85 * direction))
            .min(px(0.))
            .max(-handle.max_offset().x);
        self.paging = Some(cx.spawn(async move |this, cx| {
            const FRAMES: u32 = 15;
            for frame in 1..=FRAMES {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(16))
                    .await;
                let t = frame as f32 / FRAMES as f32;
                let eased = 1. - (1. - t).powi(3);
                handle.set_offset(point(start.x + (target - start.x) * eased, start.y));
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    fn art_ref(&self, kind: ArtKind) -> &Option<String> {
        match kind {
            ArtKind::Cover => &self.art.cover,
            ArtKind::Hero => &self.art.hero,
            ArtKind::Icon => &self.art.icon,
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
            (true, ..) => "Guardar cambios",
            (false, true, _, ImportMode::Copy) => "Copiando el prefijo...",
            (false, true, _, ImportMode::Move) => "Moviendo el prefijo...",
            (false, false, true, ImportMode::Copy) => "Copiar prefijo y agregar",
            (false, false, true, ImportMode::Move) => "Mover prefijo y agregar",
            (false, false, false, _) => "Agregar a la biblioteca",
        };
        div()
            .flex()
            .flex_col()
            .child(div().mb_3().max_w(px(520.)).child(self.name.clone()))
            .when(!self.editing, |form| form.child(self.installer_row(cx)))
            .child(
                div()
                    .flex()
                    .gap(px(10.))
                    .mb_3()
                    .max_w(px(640.))
                    .child(div().flex_1().min_w_0().child(self.executable.clone()))
                    .child(
                        button("external-pick-exe", ButtonKind::Secondary, "Elegir archivo...", false)
                            .on_click(cx.listener(|this, _, _, cx| this.pick_executable(cx))),
                    ),
            )
            .child(self.art_section(cx))
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

fn thumb(kind: ArtKind, url: &str, selected: bool) -> Div {
    let (width, height) = kind.thumb();
    div()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .rounded(px(theme::RADIUS_SM))
        .overflow_hidden()
        .border_2()
        .border_color(if selected {
            rgb(theme::ACCENT_BLUE)
        } else {
            rgb(theme::LINE)
        })
        .bg(rgb(theme::BG_PANEL))
        .child(
            img(crate::art::source(url, gpui::size(width, height)))
                .size_full()
                .object_fit(ObjectFit::Cover),
        )
}

fn empty_thumb(kind: ArtKind) -> Div {
    let (width, height) = kind.thumb();
    div()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_SM))
        .border_1()
        .border_dashed()
        .border_color(rgb(theme::LINE))
        .text_size(px(11.))
        .text_color(rgb(theme::TEXT_MUTED))
        .child("Sin imagen")
}
