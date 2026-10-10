//! What's taking up room, and how much is left — one card per library
//! folder, the way Steam's Storage settings list a drive per folder, since
//! a profile's games can be spread over several disks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use decanter_core::{InstalledGame, Library, Storefront};
use gpui::{AnyElement, Context, SharedString, Task, Window, div, prelude::*, px, rgb};

use crate::external::transfer::Progress;
use crate::state::AppState;
use crate::theme;
use crate::widgets::select::Select;
use crate::widgets::{ButtonKind, button, stat_card, tooltip};

/// One library folder as the last scan found it.
struct Folder {
    root: PathBuf,
    primary: bool,
    /// `false` for a folder on a disk that isn't plugged in.
    present: bool,
    games: Vec<InstalledGame>,
    total: u64,
    disk: Option<(u64, u64)>,
}

impl Folder {
    /// Every game recorded in `root` for the profile's accounts and for the
    /// profile itself (its external games).
    fn scan(root: PathBuf, primary: bool, owners: &[String]) -> Self {
        let present = root.is_dir();
        let library = Library::new(&root);
        let mut games: Vec<InstalledGame> = owners
            .iter()
            .flat_map(|owner| library.scan(owner).into_values())
            // An external game sharing a launcher's prefix is counted in the
            // launcher's size, not twice.
            .filter(|game| game.storefront != Storefront::Unspecified || game.prefix_of.is_none())
            .collect();
        games.sort_by_key(|game| std::cmp::Reverse(game.size_on_disk.unwrap_or(0)));
        let total = games.iter().filter_map(|game| game.size_on_disk).sum();
        let disk = present.then(|| decanter_core::disk_usage(&root)).flatten();
        Self {
            root,
            primary,
            present,
            games,
            total,
            disk,
        }
    }
}

/// Reads every folder's manifests and free space — slow enough on a big
/// library to be kept off the UI thread.
fn scan(profile_id: &str) -> Vec<Folder> {
    let db = crate::core::db();
    // What's installed is filed per account (see `decanter_core::library`);
    // external games are filed under the profile itself.
    let owners: Vec<String> = db
        .as_ref()
        .ok()
        .and_then(|db| {
            db.accounts_in_profile(profile_id)
                .inspect_err(|err| tracing::warn!(profile_id, %err, "failed to list accounts for storage view"))
                .ok()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|account| account.id)
        .chain(std::iter::once(profile_id.to_owned()))
        .collect();
    let primary = crate::core::library_for_profile(profile_id)
        .inspect_err(|err| tracing::warn!(profile_id, %err, "failed to resolve library root for storage view"))
        .ok()
        .map(|library| library.root().to_path_buf());
    // Read from the database rather than the resolved library, which leaves
    // out a folder whose disk is unplugged — that one still gets a card.
    let extra = db
        .ok()
        .and_then(|db| db.profile_extra_libraries(profile_id).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from);
    let folders: Vec<Folder> = primary
        .into_iter()
        .map(|root| (root, true))
        .chain(extra.map(|root| (root, false)))
        .map(|(root, primary)| Folder::scan(root, primary, &owners))
        .collect();
    tracing::debug!(profile_id, folders = folders.len(), "computed storage usage");
    folders
}

pub struct StorageView {
    /// `None` until the first scan for this profile lands.
    folders: Option<Vec<Folder>>,
    error: String,
    scan: Option<Task<()>>,
    /// The game being moved between folders, and how far it's got.
    moving: Option<(String, Arc<Progress>)>,
    _move: Option<Task<()>>,
    /// Redraws the progress while `moving`: the move runs on a blocking
    /// thread that can't notify this view itself.
    _ticker: Option<Task<()>>,
}

impl StorageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let session = AppState::get(cx).session.clone();
        cx.observe(&session, |this, _, cx| this.rescan(cx)).detach();
        let mut this = Self {
            folders: None,
            error: String::new(),
            scan: None,
            moving: None,
            _move: None,
            _ticker: None,
        };
        this.rescan(cx);
        this
    }

    /// Re-read on every visit, as the webview build did by remounting the
    /// page: an install or uninstall elsewhere changes what's here.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            self.folders = None;
            self.scan = None;
            cx.notify();
            return;
        };
        let scan = cx.background_spawn(async move { scan(&profile_id) });
        self.scan = Some(cx.spawn(async move |this, cx| {
            let folders = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.folders = Some(folders);
                cx.notify();
            });
        }));
    }

    /// What changed is which disks the library reads from, so the library
    /// itself re-reads what's installed, not just this page.
    fn folders_changed(&mut self, cx: &mut Context<Self>) {
        AppState::get(cx)
            .library
            .clone()
            .update(cx, |library, cx| library.refresh_all_install_state(cx));
        self.rescan(cx);
    }

    fn add_folder(&mut self, cx: &mut Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let Some(folder) = rfd::AsyncFileDialog::new()
                .set_title("Elegí dónde guardar juegos")
                .pick_folder()
                .await
            else {
                return;
            };
            let picked = folder.path().to_path_buf();
            let result = crate::core::add_library_folder(&profile_id, &picked);
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(root) => {
                        tracing::info!(profile_id, root = %root.display(), "library folder added");
                        this.error.clear();
                        this.folders_changed(cx);
                    }
                    Err(err) => this.error = err,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_folder(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return;
        };
        match crate::core::remove_library_folder(&profile_id, root) {
            Ok(()) => {
                self.error.clear();
                self.folders_changed(cx);
            }
            Err(err) => self.error = err,
        }
        cx.notify();
    }

    fn move_game(&mut self, game: &InstalledGame, to: PathBuf, cx: &mut Context<Self>) {
        if self.moving.is_some() {
            return;
        }
        let progress = Arc::new(Progress::default());
        self.moving = Some((game.game_id.clone(), Arc::clone(&progress)));
        self.error.clear();
        let (account_id, game_id) = (game.account_id.clone(), game.game_id.clone());
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let still = this.update(cx, |this, cx| {
                    cx.notify();
                    this.moving.is_some()
                });
                if !matches!(still, Ok(true)) {
                    return;
                }
            }
        }));
        self._move = Some(cx.spawn(async move |this, cx| {
            let result =
                tokio::task::spawn_blocking(move || crate::relocate::move_game(&account_id, &game_id, &to, &progress))
                    .await
                    .unwrap_or_else(|err| Err(err.to_string()));
            let _ = this.update(cx, |this, cx| {
                this.moving = None;
                if let Err(err) = result {
                    this.error = err;
                }
                this.folders_changed(cx);
            });
        }));
        cx.notify();
    }

    /// "Mover a…" for a game, or how far its move has got.
    fn move_control(&self, id: usize, game: &InstalledGame, from: &Path, cx: &Context<Self>) -> Option<AnyElement> {
        if let Some((moving, progress)) = &self.moving
            && *moving == game.game_id
        {
            let text = match progress.snapshot() {
                Some((done, total)) => format!(
                    "Moviendo {} de {}",
                    crate::format::bytes(done),
                    crate::format::bytes(total)
                ),
                None => "Moviendo...".to_owned(),
            };
            return Some(theme::muted(text).text_size(px(12.)).into_any_element());
        }
        // A game a launcher installed moves with its launcher.
        if game.prefix_of.is_some() || self.moving.is_some() {
            return None;
        }
        let targets: Vec<PathBuf> = self
            .folders
            .iter()
            .flatten()
            .filter(|folder| folder.present && folder.root != from)
            .map(|folder| folder.root.clone())
            .collect();
        if targets.is_empty() {
            return None;
        }
        let options = std::iter::once((SharedString::default(), SharedString::from("Mover a…"))).chain(
            targets.iter().map(|root| {
                let path = SharedString::from(root.to_string_lossy().into_owned());
                (path.clone(), path)
            }),
        );
        let game = game.clone();
        let this = cx.entity().downgrade();
        Some(
            Select::new(("storage-move", id), options, SharedString::default())
                .on_change(move |value, _, cx| {
                    if value.is_empty() {
                        return;
                    }
                    let to = PathBuf::from(value.as_ref());
                    let _ = this.update(cx, |this, cx| this.move_game(&game, to, cx));
                })
                .into_any_element(),
        )
    }

    fn folder_card(&self, index: usize, folder: &Folder, cx: &Context<Self>) -> impl IntoElement + use<> {
        let label = if folder.primary { "Principal" } else { "Adicional" };
        let status = match (folder.present, folder.disk) {
            (false, _) => "Disco no conectado".to_owned(),
            (true, Some((free, total))) => format!(
                "{} usados por {} juego(s) · {} libres de {}",
                crate::format::bytes(folder.total),
                folder.games.len(),
                crate::format::bytes(free),
                crate::format::bytes(total)
            ),
            (true, None) => format!(
                "{} usados por {} juego(s)",
                crate::format::bytes(folder.total),
                folder.games.len()
            ),
        };
        // A folder with games still in it would take them out of the
        // library without moving them anywhere. An unplugged one can't be
        // checked, and is the folder most likely to be gone for good.
        let removable = !folder.primary && (folder.games.is_empty() || !folder.present);
        let root = folder.root.clone();
        let count = folder.games.len();
        theme::card()
            .mb_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .px(px(18.))
                    .py(px(14.))
                    .border_b_1()
                    .border_color(rgb(theme::LINE))
                    .child(theme::badge(label, folder.primary))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(folder.root.display().to_string())
                            .child(theme::muted(status).text_size(px(12.))),
                    )
                    .when(!folder.primary, |row| {
                        row.child(
                            button(("storage-remove", index), ButtonKind::Icon, "Quitar", !removable)
                                .tooltip(tooltip(if removable {
                                    "Deja de usar esta carpeta; lo que haya en el disco se queda"
                                } else {
                                    "Tiene juegos: desinstalalos o movelos antes"
                                }))
                                .when(removable, |b| {
                                    b.on_click(cx.listener(move |this, _, _, cx| this.remove_folder(&root, cx)))
                                }),
                        )
                    }),
            )
            .children(folder.games.iter().enumerate().map(|(row, game)| {
                let id = index * 10_000 + row;
                theme::list_row(("storage-row", id), row + 1 == count)
                    .child(div().flex_1().child(game.name.clone()))
                    .children(self.move_control(id, game, &folder.root, cx))
                    .child(
                        div()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(crate::format::bytes(game.size_on_disk.unwrap_or(0))),
                    )
            }))
    }
}

impl Render for StorageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let column = div().flex().flex_col().child(theme::h1("Almacenamiento"));
        if AppState::get(cx).session.read(cx).active_profile.is_none() {
            return theme::page("storage", column.child(theme::empty("Elegi un perfil arriba.")));
        }
        let Some(folders) = &self.folders else {
            return theme::page("storage", column);
        };
        let (games, used) = folders.iter().fold((0, 0), |(games, used), folder| {
            (games + folder.games.len(), used + folder.total)
        });

        let stats = div()
            .flex()
            .flex_wrap()
            .gap(px(14.))
            .mt(px(20.))
            .mb_6()
            .child(stat_card(
                crate::format::bytes(used),
                &format!("usado por {games} juego(s)"),
            ))
            .child(stat_card(folders.len().to_string(), "carpeta(s) de biblioteca"));

        theme::page(
            "storage",
            column
                .child(stats)
                .children(
                    folders
                        .iter()
                        .enumerate()
                        .map(|(index, folder)| self.folder_card(index, folder, cx)),
                )
                .child(
                    div().flex().child(
                        button(
                            "storage-add-folder",
                            ButtonKind::Secondary,
                            "Agregar carpeta de biblioteca",
                            false,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                    ),
                )
                .when(!self.error.is_empty(), |column| {
                    column.child(div().mt_3().child(theme::error(self.error.clone())))
                }),
        )
    }
}
