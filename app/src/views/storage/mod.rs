//! What's taking up room, and how much is left — one card per library
//! folder, the way Steam's Storage settings list a drive per folder, since
//! a profile's games can be spread over several disks.

mod moves;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use decanter_core::{InstalledGame, Library, Storefront};
use gpui::{Context, Entity, Task, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::i18n::Text;
use crate::state::{AppState, Download, DownloadKey, MoveFinished, MoveKey, MoveQueue};
use crate::theme;
use crate::views::downloads;
use crate::widgets::reveal::reveal;
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
    /// Folders in it with no journal, which nothing else lists.
    strays: Vec<decanter_gog::StrayStaging>,
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
            // Steam keeps a Steam entry's files, in its own library folders.
            .filter(|game| game.storefront != Storefront::Steam)
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
        let strays = if present {
            decanter_gog::stray_staging(&root)
        } else {
            Vec::new()
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
            strays,
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
    /// Mounted removable drives and SD cards, offered as places to add.
    mounts: Vec<PathBuf>,
    error: Option<Text>,
    scan: Option<Task<()>>,
    /// Games and dumps moving one after another; it outlives this page.
    queue: Entity<MoveQueue>,
    /// The games and dumps picked with their checkboxes to move together.
    selected: HashSet<MoveKey>,
    /// The folder decanter kept everything in before it moved into the
    /// library, while it's still there.
    legacy: Option<PathBuf>,
    confirm_legacy: bool,
    /// The journal-less staging folder whose Eliminar waits for its
    /// confirmation: library folder, account and folder name.
    confirm_stray: Option<(PathBuf, String, String)>,
}

impl StorageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.session, |this, _, cx| this.rescan(cx)).detach();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.moves, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&state.moves, |this, _, _: &MoveFinished, cx| this.rescan(cx))
            .detach();
        let mut this = Self {
            folders: None,
            mounts: Vec::new(),
            error: None,
            scan: None,
            queue: state.moves.clone(),
            selected: HashSet::new(),
            legacy: None,
            confirm_legacy: false,
            confirm_stray: None,
        };
        this.rescan(cx);
        this
    }

    /// Re-read on every visit, as the webview build did by remounting the
    /// page: an install or uninstall elsewhere changes what's here.
    pub fn rescan(&mut self, cx: &mut Context<Self>) {
        self.legacy = decanter_core::migrate::legacy_leftover();
        if let Some(refused) = self.queue.update(cx, |queue, _| queue.take_refused()) {
            self.error = Some(refused);
        }
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            self.folders = None;
            self.scan = None;
            cx.notify();
            return;
        };
        let scan = cx.background_spawn(async move { (scan(&profile_id), decanter_core::removable_mounts()) });
        self.scan = Some(cx.spawn(async move |this, cx| {
            let (folders, mounts) = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.folders = Some(folders);
                this.mounts = mounts;
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
            let _ = this.update(cx, |this, cx| this.add_picked(&profile_id, &picked, cx));
        })
        .detach();
    }

    fn add_picked(&mut self, profile_id: &str, picked: &Path, cx: &mut Context<Self>) {
        match crate::core::add_library_folder(profile_id, picked) {
            Ok(root) => {
                tracing::info!(profile_id, root = %root.display(), "library folder added");
                self.error = None;
                self.folders_changed(cx);
            }
            Err(err) => self.error = Some(err),
        }
        cx.notify();
    }

    fn mount_suggestions(&self, folders: &[Folder], cx: &Context<Self>) -> Vec<impl IntoElement + use<>> {
        self.mounts
            .iter()
            .filter(|mount| {
                !folders
                    .iter()
                    .any(|folder| folder.root.starts_with(mount) || mount.starts_with(&folder.root))
            })
            .enumerate()
            .map(|(index, mount)| {
                let picked = mount.clone();
                button(
                    ("storage-add-mount", index),
                    ButtonKind::Secondary,
                    t!("storage.add_mount", path = mount.display()),
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
                        return;
                    };
                    this.add_picked(&profile_id, &picked, cx);
                }))
            })
            .collect()
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
        reveal(
            div()
                .id(("storage-cache", index))
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
                ),
        )
    }

    /// Descargas sin terminar: what the folder's `downloading/` holds, and
    /// the downloads in it with what can be done with each — the same
    /// buttons as in Descargas, the deleting one behind the same question.
    fn unfinished_rows(&self, index: usize, folder: &Folder, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let background = AppState::get(cx).background.read(cx);
        let mut rows: Vec<(&DownloadKey, &Download)> = background
            .installing
            .iter()
            .filter(|(_, row)| row.root.as_deref() == Some(folder.root.as_path()))
            .collect();
        if rows.is_empty() && folder.strays.is_empty() && folder.unfinished_size == 0 {
            return None;
        }
        rows.sort_by(|left, right| (&left.1.title, left.0).cmp(&(&right.1.title, right.0)));
        let listed = rows.into_iter().enumerate().map(|(row, (key, download))| {
            let id = index * 1_000 + row;
            let confirming = background.cancel_confirm.as_ref() == Some(key);
            reveal(
                div()
                    .id(("storage-unfinished", id))
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
                    ),
            )
        });
        let strays = folder
            .strays
            .iter()
            .enumerate()
            .map(|(row, stray)| self.stray_row(index * 1_000 + row, &folder.root, stray, cx));
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
                .children(listed)
                .children(strays),
        )
    }

    /// A staging folder with no journal: what it is and its size, and Eliminar
    /// behind a question that names it (#1133).
    fn stray_row(
        &self,
        id: usize,
        root: &Path,
        stray: &decanter_gog::StrayStaging,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let size = crate::format::bytes(stray.size);
        let pending = (root.to_path_buf(), stray.account_id.clone(), stray.name.clone());
        let confirming = self.confirm_stray.as_ref() == Some(&pending);
        let name = format!("{}/{}", stray.account_id, stray.name);
        let (root, account_id, folder) = pending.clone();
        let actions = if confirming {
            div()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(6.))
                .max_w(px(320.))
                .child(div().text_size(px(12.)).child(t!(
                    "storage.stray.confirm",
                    name = name.as_str(),
                    size = size.as_str()
                )))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            button(
                                ("stray-keep", id),
                                ButtonKind::Secondary,
                                t!("storage.stray.keep"),
                                false,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_stray = None;
                                cx.notify();
                            })),
                        )
                        .child(
                            button(
                                ("stray-delete", id),
                                ButtonKind::Icon,
                                t!("storage.stray.delete"),
                                false,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_stray(root.clone(), account_id.clone(), folder.clone(), cx);
                            })),
                        ),
                )
        } else {
            div().flex().gap_2().child(
                button(("stray-ask", id), ButtonKind::Icon, t!("storage.stray.delete"), false).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.confirm_stray = Some(pending.clone());
                        cx.notify();
                    },
                )),
            )
        };
        reveal(
            div()
                .id(("storage-stray", id))
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
                        .child(div().truncate().child(name))
                        .child(theme::muted(t!("storage.stray.note", size = size.as_str())).text_size(px(11.))),
                )
                .child(actions),
        )
    }

    fn remove_stray(&mut self, root: PathBuf, account_id: String, name: String, cx: &mut Context<Self>) {
        self.confirm_stray = None;
        cx.spawn(async move |this, cx| {
            let result = decanter_gog::discard_stray(&root, &account_id, &name)
                .await
                .inspect_err(|err| {
                    tracing::error!(root = %root.display(), account_id, name, %err, "deleting a staging folder failed");
                });
            let _ = this.update(cx, |this, cx| {
                this.error = result
                    .err()
                    .map(|err| Text::new("storage.errors.stray").arg("error", err));
                this.rescan(cx);
            });
        })
        .detach();
        cx.notify();
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
        theme::card().mt_6().child(reveal(
            div()
                .id("storage-legacy")
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
        ))
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
            .child(reveal(
                div()
                    .id(("storage-folder", index))
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
            ))
            .child(Self::cache_row(index, folder, cx))
            .children(self.unfinished_rows(index, folder, cx))
            .children(folder.games.iter().enumerate().map(|(row, game)| {
                let id = index * 10_000 + row;
                reveal(
                    theme::list_row(("storage-row", id), row + 1 == count)
                        .children(self.game_select_box(id, game, cx))
                        .child(div().flex_1().child(game.name.clone()))
                        .children(self.game_move_control(id, game, &folder.root, cx))
                        .child(
                            div()
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child(crate::format::bytes(game.size_on_disk.unwrap_or(0))),
                        ),
                )
            }))
            .children(folder.dumps.iter().enumerate().map(|(row, dump)| {
                let row = folder.games.len() + row;
                let id = index * 10_000 + row;
                reveal(
                    theme::list_row(("storage-row", id), row + 1 == count)
                        .child(self.dump_select_box(id, dump, cx))
                        .child(theme::badge("PS", false))
                        .child(div().flex_1().min_w_0().truncate().child(dump.name.clone()))
                        .children(self.dump_move_control(id, dump, &folder.root, cx))
                        .child(
                            div()
                                .text_color(rgb(theme::TEXT_MUTED))
                                .child(crate::format::bytes(dump.size)),
                        ),
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
                .children(self.selection_bar(cx))
                .children(self.queue_card(cx))
                .children(
                    folders
                        .iter()
                        .enumerate()
                        .map(|(index, folder)| self.folder_card(index, folder, cx)),
                )
                .child(reveal(
                    div()
                        .id("storage-add")
                        .flex()
                        .flex_wrap()
                        .gap_3()
                        .child(
                            button("storage-add-folder", ButtonKind::Secondary, t!("storage.add"), false)
                                .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                        )
                        .children(self.mount_suggestions(folders, cx)),
                ))
                .children(self.error.as_ref().map(|error| div().mt_3().child(theme::error(error))))
                .children(self.legacy.as_deref().map(|legacy| self.legacy_card(legacy, cx))),
        )
    }
}
