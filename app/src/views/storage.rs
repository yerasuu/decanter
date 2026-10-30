//! What's taking up room, and how much is left — one card per library
//! folder, the way Steam's Storage settings list a drive per folder, since
//! a profile's games can be spread over several disks.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use decanter_core::{InstalledGame, Library, Storefront};
use gpui::{AnyElement, Context, SharedString, Task, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::external::transfer::Progress;
use crate::i18n::Text;
use crate::state::{AppState, Download, DownloadKey, Ps};
use crate::theme;
use crate::views::downloads;
use crate::widgets::select::Select;
use crate::widgets::{ButtonKind, button, stat_card, tooltip};

/// One library folder as the last scan found it.
struct Folder {
    root: PathBuf,
    primary: bool,
    /// `false` for a folder on a disk that isn't plugged in.
    present: bool,
    games: Vec<InstalledGame>,
    /// PS dumps in its `psport/games`, which the profile reads from every
    /// folder of its library.
    dumps: Vec<Dump>,
    total: u64,
    disk: Option<(u64, u64)>,
    /// Where its games' shader caches go, whether the user picked it, and
    /// how much is in it (`None` before anything was written there).
    cache: PathBuf,
    cache_custom: bool,
    cache_size: Option<u64>,
    /// What its `downloading/` holds: downloads left unfinished, and
    /// whatever is downloading right now.
    unfinished_size: u64,
}

struct Dump {
    dir: PathBuf,
    name: String,
    size: u64,
}

impl Dump {
    fn scan(root: &Path, db: Option<&decanter_core::Db>) -> Vec<Self> {
        let games = decanter_ps::paths::games_in(root);
        if !games.is_dir() {
            return Vec::new();
        }
        decanter_ps::library::scan(std::slice::from_ref(&games))
            .into_iter()
            .map(|game| {
                let size = crate::dump_size::size(db, &game.dir);
                let title = if game.title.is_empty() {
                    &game.title_id
                } else {
                    &game.title
                };
                Self {
                    name: format!("{title} ({})", game.version),
                    size,
                    dir: game.dir,
                }
            })
            .collect()
    }
}

impl Folder {
    /// Every game recorded in `root` for the profile's accounts and for the
    /// profile itself (its external games).
    fn scan(root: PathBuf, primary: bool, owners: &[String], db: Option<&decanter_core::Db>) -> Self {
        let present = root.is_dir();
        let library = Library::new(&root);
        let mut games: Vec<InstalledGame> = owners
            .iter()
            .flat_map(|owner| library.scan(owner).into_values())
            // An external game sharing a launcher's prefix is counted in the
            // launcher's size, not twice.
            .filter(|game| game.storefront != Storefront::Unspecified || game.prefix_of.is_none())
            // Checked on every visit, since nothing else learns when an
            // external game's files change; walked only when they did.
            .map(|game| {
                if present && game.storefront == Storefront::Unspecified {
                    crate::external::size::refresh(&library, game)
                } else {
                    game
                }
            })
            .collect();
        games.sort_by_key(|game| std::cmp::Reverse(game.size_on_disk.unwrap_or(0)));
        let mut dumps = if present { Dump::scan(&root, db) } else { Vec::new() };
        dumps.sort_by_key(|dump| std::cmp::Reverse(dump.size));
        let total = games
            .iter()
            .filter_map(|game| game.size_on_disk)
            .chain(dumps.iter().map(|dump| dump.size))
            .sum();
        let disk = present.then(|| decanter_core::disk_usage(&root)).flatten();
        let (cache, cache_custom) = crate::cache::location(&root);
        let cache_size = cache.is_dir().then(|| {
            decanter_core::directory_size(&cache)
                .inspect_err(|err| tracing::warn!(dir = %cache.display(), %err, "could not measure the cache"))
                .unwrap_or(0)
        });
        let downloading = decanter_gog::downloading_dir(&root);
        let unfinished_size = if present && downloading.is_dir() {
            decanter_core::directory_size(&downloading)
                .inspect_err(
                    |err| tracing::warn!(dir = %downloading.display(), %err, "could not measure unfinished downloads"),
                )
                .unwrap_or(0)
        } else {
            0
        };
        Self {
            root,
            primary,
            present,
            games,
            dumps,
            total,
            disk,
            cache,
            cache_custom,
            cache_size,
            unfinished_size,
        }
    }
}

/// Reads every folder's manifests and free space — slow enough on a big
/// library to be kept off the UI thread.
fn scan(profile_id: &str) -> Vec<Folder> {
    let db = crate::core::db().ok();
    // What's installed is filed per account (see `decanter_core::library`);
    // external games are filed under the profile itself.
    let owners: Vec<String> = db
        .as_ref()
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
        .as_ref()
        .and_then(|db| db.profile_extra_libraries(profile_id).ok())
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from);
    let folders: Vec<Folder> = primary
        .into_iter()
        .map(|root| (root, true))
        .chain(extra.map(|root| (root, false)))
        .map(|(root, primary)| Folder::scan(root, primary, &owners, db.as_ref()))
        .collect();
    tracing::debug!(profile_id, folders = folders.len(), "computed storage usage");
    folders
}

pub struct StorageView {
    /// `None` until the first scan for this profile lands.
    folders: Option<Vec<Folder>>,
    error: Option<Text>,
    scan: Option<Task<()>>,
    /// The game being moved between folders (a GOG or external game's id,
    /// or a PS dump's folder), and how far it's got.
    moving: Option<(String, Arc<Progress>)>,
    _move: Option<Task<()>>,
    /// Redraws the progress while `moving`: the move runs on a blocking
    /// thread that can't notify this view itself.
    _ticker: Option<Task<()>>,
    /// The folder decanter kept everything in before it moved into the
    /// library, while it's still there.
    legacy: Option<PathBuf>,
    confirm_legacy: bool,
}

impl StorageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.session, |this, _, cx| this.rescan(cx)).detach();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        let mut this = Self {
            folders: None,
            error: None,
            scan: None,
            moving: None,
            _move: None,
            _ticker: None,
            legacy: None,
            confirm_legacy: false,
        };
        this.rescan(cx);
        this
    }

    /// Re-read on every visit, as the webview build did by remounting the
    /// page: an install or uninstall elsewhere changes what's here.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.legacy = decanter_core::migrate::legacy_leftover();
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
                .set_title(t!("storage.add_title"))
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
                        this.error = None;
                        this.folders_changed(cx);
                    }
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Lets the user pick where `folder`'s caches go, or without `pick` puts
    /// them back inside it.
    fn change_cache(folder: PathBuf, pick: bool, cx: &Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let picked = if pick {
                let Some(picked) = rfd::AsyncFileDialog::new()
                    .set_title(t!("storage.cache.pick_title"))
                    .pick_folder()
                    .await
                else {
                    return;
                };
                Some(picked.path().to_path_buf())
            } else {
                None
            };
            let result = cx
                .background_spawn(async move { crate::cache::set(&profile_id, &folder, picked.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.error = None;
                        this.rescan(cx);
                    }
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cache_row(index: usize, folder: &Folder, cx: &Context<Self>) -> impl IntoElement + use<> {
        let size = folder
            .cache_size
            .map_or_else(|| t!("storage.cache.empty").into_owned(), crate::format::bytes);
        let (pick_root, reset_root) = (folder.root.clone(), folder.root.clone());
        div()
            .flex()
            .items_center()
            .gap_4()
            .px(px(18.))
            .py(px(10.))
            .border_b_1()
            .border_color(rgb(theme::LINE))
            .child(
                div()
                    .w(px(70.))
                    .flex_none()
                    .text_color(rgb(theme::TEXT_MUTED))
                    .child(t!("storage.cache.title")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(theme::mono().truncate().child(folder.cache.display().to_string()))
                    .child(
                        theme::muted(if folder.cache_custom {
                            t!("storage.cache.custom", size = size)
                        } else {
                            t!("storage.cache.default", size = size)
                        })
                        .text_size(px(12.)),
                    ),
            )
            .when(folder.cache_custom, |row| {
                row.child(
                    button(
                        ("storage-cache-reset", index),
                        ButtonKind::Secondary,
                        t!("storage.cache.reset"),
                        false,
                    )
                    .tooltip(tooltip(t!("storage.cache.reset_hint")))
                    .on_click(cx.listener(move |_, _, _, cx| Self::change_cache(reset_root.clone(), false, cx))),
                )
            })
            .child(
                button(
                    ("storage-cache-pick", index),
                    ButtonKind::Secondary,
                    t!("storage.cache.change"),
                    !folder.present,
                )
                .tooltip(tooltip(t!("storage.cache.change_hint")))
                .when(folder.present, |b| {
                    b.on_click(cx.listener(move |_, _, _, cx| Self::change_cache(pick_root.clone(), true, cx)))
                }),
            )
    }

    /// Descargas sin terminar: what the folder's `downloading/` holds, and
    /// the downloads in it with what can be done with each — the same
    /// buttons as in Descargas, the deleting one behind the same question.
    fn unfinished_rows(index: usize, folder: &Folder, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let background = AppState::get(cx).background.read(cx);
        let mut rows: Vec<(&DownloadKey, &Download)> = background
            .installing
            .iter()
            .filter(|(_, row)| row.root.as_deref() == Some(folder.root.as_path()))
            .collect();
        if rows.is_empty() && folder.unfinished_size == 0 {
            return None;
        }
        rows.sort_by(|left, right| (&left.1.title, left.0).cmp(&(&right.1.title, right.0)));
        let listed = rows.into_iter().enumerate().map(|(row, (key, download))| {
            let id = index * 1_000 + row;
            let confirming = background.cancel_confirm.as_ref() == Some(key);
            div()
                .flex()
                .items_start()
                .gap_4()
                .pt_2()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(download.title.clone())
                        .children(downloads::progress_lines(key, download)),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .gap_2()
                        .children(downloads::download_actions(id, key, download, confirming)),
                )
        });
        Some(
            div()
                .flex()
                .flex_col()
                .px(px(18.))
                .py(px(10.))
                .border_b_1()
                .border_color(rgb(theme::LINE))
                .child(
                    div()
                        .flex()
                        .gap_4()
                        .child(
                            div()
                                .flex_1()
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child(t!("storage.unfinished.title")),
                        )
                        .child(
                            div()
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child(crate::format::bytes(folder.unfinished_size)),
                        ),
                )
                .children(listed),
        )
    }

    fn remove_folder(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return;
        };
        match crate::core::remove_library_folder(&profile_id, root) {
            Ok(()) => {
                self.error = None;
                self.folders_changed(cx);
            }
            Err(err) => self.error = Some(err),
        }
        cx.notify();
    }

    fn remove_legacy(&mut self, cx: &mut Context<Self>) {
        self.confirm_legacy = false;
        let removal = cx.background_spawn(async {
            // Every profile's folders, an unplugged one too: the old default
            // library lived inside the old data folder.
            let libraries: Vec<PathBuf> = crate::core::db()
                .and_then(|db| {
                    let profiles = db.list_profiles().map_err(crate::core::StoreError::Query)?;
                    Ok(profiles
                        .iter()
                        .flat_map(|profile| {
                            let extra = db.profile_extra_libraries(&profile.id).unwrap_or_default();
                            crate::core::library_for_profile(&profile.id)
                                .map(|library| library.roots())
                                .unwrap_or_default()
                                .into_iter()
                                .chain(extra.into_iter().map(PathBuf::from))
                        })
                        .collect())
                })
                .unwrap_or_default();
            decanter_core::migrate::remove_legacy(&libraries)
        });
        cx.spawn(async move |this, cx| {
            let result = removal.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.error = None,
                    Err(decanter_core::migrate::Error::Busy) => {
                        this.error = Some(Text::new("storage.errors.legacy_busy"));
                    }
                    Err(err) => {
                        tracing::error!(%err, "cannot remove the old data folder");
                        this.error = Some(Text::new("storage.errors.legacy").arg("error", err));
                    }
                }
                this.legacy = decanter_core::migrate::legacy_leftover();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn legacy_card(&self, legacy: &Path, cx: &Context<Self>) -> impl IntoElement + use<> {
        let label = if self.confirm_legacy {
            t!("storage.legacy.confirm")
        } else {
            t!("storage.legacy.remove")
        };
        theme::card().mt_6().child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .px(px(18.))
                .py(px(14.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(t!("storage.legacy.title"))
                        .child(theme::muted(t!("storage.legacy.body", path = legacy.display())).text_size(px(12.))),
                )
                .child(
                    button("storage-remove-legacy", ButtonKind::Secondary, label, false).on_click(cx.listener(
                        |this, _, _, cx| {
                            if this.confirm_legacy {
                                this.remove_legacy(cx);
                            } else {
                                this.confirm_legacy = true;
                                cx.notify();
                            }
                        },
                    )),
                ),
        )
    }

    fn move_game(&mut self, game: &InstalledGame, to: PathBuf, cx: &mut Context<Self>) {
        if self.moving.is_some() {
            return;
        }
        let progress = Arc::new(Progress::default());
        let (account_id, game_id) = (game.account_id.clone(), game.game_id.clone());
        let work = {
            let progress = Arc::clone(&progress);
            async move {
                tokio::task::spawn_blocking(move || crate::relocate::move_game(&account_id, &game_id, &to, &progress))
                    .await
                    .unwrap_or_else(|err| Err(Text::verbatim(err)))
            }
        };
        self.track_move(game.game_id.clone(), progress, work, cx);
    }

    fn move_dump(&mut self, dir: PathBuf, to: PathBuf, cx: &mut Context<Self>) {
        if self.moving.is_some() {
            return;
        }
        let progress = Arc::new(Progress::default());
        let key = dir.to_string_lossy().into_owned();
        let work = Ps::get(cx).update(cx, |ps, cx| ps.move_dump(dir, to, Arc::clone(&progress), cx));
        self.track_move(key, progress, work, cx);
    }

    /// Shows `work`'s progress under `key` until it ends, then reads the
    /// folders again.
    fn track_move(
        &mut self,
        key: String,
        progress: Arc<Progress>,
        work: impl Future<Output = Result<(), Text>> + 'static,
        cx: &mut Context<Self>,
    ) {
        self.moving = Some((key, progress));
        self.error = None;
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
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.moving = None;
                if let Err(err) = result {
                    this.error = Some(err);
                }
                this.folders_changed(cx);
            });
        }));
        cx.notify();
    }

    /// "Mover a…" for a game or dump, or how far its move has got. `start`
    /// moves it into the library folder picked.
    fn move_control(
        &self,
        id: usize,
        key: &str,
        from: &Path,
        start: impl Fn(&mut Self, PathBuf, &mut Context<Self>) + 'static,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if let Some((moving, progress)) = &self.moving
            && moving == key
        {
            let text = progress.snapshot().map_or_else(
                || t!("storage.moving.started"),
                |(done, total)| {
                    t!(
                        "storage.moving.progress",
                        done = crate::format::bytes(done),
                        total = crate::format::bytes(total)
                    )
                },
            );
            return Some(theme::muted(text).text_size(px(12.)).into_any_element());
        }
        if self.moving.is_some() {
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
        let options = std::iter::once((SharedString::default(), t!("storage.move_to").into())).chain(
            targets.iter().map(|root| {
                let path = SharedString::from(root.to_string_lossy().into_owned());
                (path.clone(), path)
            }),
        );
        let this = cx.entity().downgrade();
        Some(
            Select::new(("storage-move", id), options, SharedString::default())
                .on_change(move |value, _, cx| {
                    if value.is_empty() {
                        return;
                    }
                    let to = PathBuf::from(value.as_ref());
                    let _ = this.update(cx, |this, cx| start(this, to, cx));
                })
                .into_any_element(),
        )
    }

    fn game_move_control(
        &self,
        id: usize,
        game: &InstalledGame,
        from: &Path,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        // A game a launcher installed moves with its launcher.
        if game.prefix_of.is_some() {
            return None;
        }
        let owned = game.clone();
        self.move_control(
            id,
            &game.game_id,
            from,
            move |this, to, cx| this.move_game(&owned, to, cx),
            cx,
        )
    }

    fn dump_move_control(&self, id: usize, dump: &Dump, from: &Path, cx: &Context<Self>) -> Option<AnyElement> {
        let dir = dump.dir.clone();
        self.move_control(
            id,
            &dump.dir.to_string_lossy(),
            from,
            move |this, to, cx| this.move_dump(dir.clone(), to, cx),
            cx,
        )
    }

    fn folder_card(&self, index: usize, folder: &Folder, cx: &Context<Self>) -> impl IntoElement + use<> {
        let label = if folder.primary {
            t!("storage.folder.primary")
        } else {
            t!("storage.folder.extra")
        };
        let size = crate::format::bytes(folder.total);
        let count = folder.games.len() + folder.dumps.len();
        let used = if count == 1 {
            t!("storage.folder.used.one", used = size)
        } else {
            t!("storage.folder.used.other", used = size, count = count)
        };
        let status = match (folder.present, folder.disk) {
            (false, _) => t!("storage.folder.offline").into_owned(),
            (true, Some((free, total))) => format!(
                "{used} · {}",
                t!(
                    "storage.folder.free",
                    free = crate::format::bytes(free),
                    total = crate::format::bytes(total)
                )
            ),
            (true, None) => used.into_owned(),
        };
        // A folder with games still in it would take them out of the
        // library without moving them anywhere. An unplugged one can't be
        // checked, and is the folder most likely to be gone for good.
        let removable = !folder.primary && (count == 0 || !folder.present);
        let root = folder.root.clone();
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
                            button(
                                ("storage-remove", index),
                                ButtonKind::Icon,
                                t!("storage.folder.remove"),
                                !removable,
                            )
                            .tooltip(tooltip(if removable {
                                t!("storage.folder.remove_hint")
                            } else {
                                t!("storage.folder.remove_blocked")
                            }))
                            .when(removable, |b| {
                                b.on_click(cx.listener(move |this, _, _, cx| this.remove_folder(&root, cx)))
                            }),
                        )
                    }),
            )
            .child(Self::cache_row(index, folder, cx))
            .children(Self::unfinished_rows(index, folder, cx))
            .children(folder.games.iter().enumerate().map(|(row, game)| {
                let id = index * 10_000 + row;
                theme::list_row(("storage-row", id), row + 1 == count)
                    .child(div().flex_1().child(game.name.clone()))
                    .children(self.game_move_control(id, game, &folder.root, cx))
                    .child(
                        div()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(crate::format::bytes(game.size_on_disk.unwrap_or(0))),
                    )
            }))
            .children(folder.dumps.iter().enumerate().map(|(row, dump)| {
                let row = folder.games.len() + row;
                let id = index * 10_000 + row;
                theme::list_row(("storage-row", id), row + 1 == count)
                    .child(theme::badge("PS", false))
                    .child(div().flex_1().min_w_0().truncate().child(dump.name.clone()))
                    .children(self.dump_move_control(id, dump, &folder.root, cx))
                    .child(
                        div()
                            .text_color(rgb(theme::TEXT_MUTED))
                            .child(crate::format::bytes(dump.size)),
                    )
            }))
    }
}

impl Render for StorageView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let column = div().flex().flex_col().child(theme::h1(t!("storage.title")));
        if AppState::get(cx).session.read(cx).active_profile.is_none() {
            return theme::page("storage", column.child(theme::empty(t!("common.pick_profile"))));
        }
        let Some(folders) = &self.folders else {
            return theme::page("storage", column);
        };
        let (games, used) = folders.iter().fold((0, 0), |(games, used), folder| {
            (games + folder.games.len() + folder.dumps.len(), used + folder.total)
        });

        let stats = div()
            .flex()
            .flex_wrap()
            .gap(px(14.))
            .mt(px(20.))
            .mb_6()
            .child(stat_card(
                crate::format::bytes(used),
                &if games == 1 {
                    t!("storage.stats.used.one", count = games)
                } else {
                    t!("storage.stats.used.other", count = games)
                },
            ))
            .child(stat_card(
                folders.len().to_string(),
                &if folders.len() == 1 {
                    t!("storage.stats.folders.one")
                } else {
                    t!("storage.stats.folders.other")
                },
            ));

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
                        button("storage-add-folder", ButtonKind::Secondary, t!("storage.add"), false)
                            .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                    ),
                )
                .children(self.error.as_ref().map(|error| div().mt_3().child(theme::error(error))))
                .children(self.legacy.as_deref().map(|legacy| self.legacy_card(legacy, cx))),
        )
    }
}
