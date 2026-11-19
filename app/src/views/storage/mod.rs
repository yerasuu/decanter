//! What's taking up room, and how much is left — one card per library
//! folder, the way Steam's Storage settings list a drive per folder, since
//! a profile's games can be spread over several disks.

mod moves;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use decanter_core::{InstalledGame, Library, Storefront};
use gpui::{Context, Entity, Task, Window, div, prelude::*, px, rgb};
use rust_i18n::t;

use crate::i18n::Text;
use crate::state::{AppState, Download, DownloadKey, MoveFinished, MoveKey, MoveQueue};
use crate::theme;
use crate::views::downloads;
use crate::widgets::reveal::reveal;
use crate::widgets::{ButtonKind, button, checkbox, progress_bar, tooltip};

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
    /// Steam's apps installed in this folder: not in `total`, since Steam
    /// owns their files.
    steam_games: usize,
    steam_size: u64,
    is_default: bool,
    /// Kept out of the Library views, and out of this page unless it shows hidden folders.
    hidden: bool,
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
        let (steam_games, steam_size) = if present { steam_part(&root) } else { (0, 0) };
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
            steam_games,
            steam_size,
            is_default: false,
            hidden: false,
            disk,
            cache,
            cache_custom,
            cache_size,
            unfinished_size,
            strays,
        }
    }
}

/// Count and size of the Steam apps whose library is this folder, which Steam
/// registered in its `libraryfolders.vdf`.
fn steam_part(root: &Path) -> (usize, u64) {
    let Some(steam_root) = decanter_core::steam::steam_root() else {
        return (0, 0);
    };
    let Ok(root) = root.canonicalize() else {
        return (0, 0);
    };
    decanter_core::steam::installed_apps(&steam_root)
        .into_iter()
        .filter(|app| {
            app.folder
                .ancestors()
                .nth(2)
                .and_then(|steamapps| steamapps.canonicalize().ok())
                .is_some_and(|steamapps| steamapps == root)
        })
        .fold((0, 0), |(games, size), app| {
            let measured = decanter_core::directory_size(&app.folder)
                .inspect_err(|err| tracing::warn!(dir = %app.folder.display(), %err, "could not measure a Steam app"))
                .unwrap_or(0);
            (games + 1, size + measured)
        })
}

/// Whether Storage lists the folders hidden from the Library, per profile.
const SHOW_HIDDEN_FOLDERS: &str = "show_hidden_libraries";

/// What one visit to the page found.
struct Scan {
    folders: Vec<Folder>,
    /// Hidden folders left out of `folders`, never measured, so a mount
    /// already in the library isn't offered again.
    left_out: Vec<PathBuf>,
    /// How many of the profile's folders are hidden, listed or not.
    hidden: usize,
    show_hidden: bool,
}

/// Reads every folder's manifests and free space — slow enough on a big
/// library to be kept off the UI thread. A hidden folder is left out unless
/// the page shows hidden ones, except the default install target or a lone
/// folder, which new installs go to.
fn scan(profile_id: &str) -> Scan {
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
    let roots: Vec<(PathBuf, bool)> = primary
        .into_iter()
        .map(|root| (root, true))
        .chain(extra.map(|root| (root, false)))
        .collect();
    let default = crate::core::default_install_folder(profile_id);
    let hidden_roots = crate::core::hidden_folders(profile_id);
    let show_hidden = db.as_ref().is_some_and(|db| {
        db.setting(&format!("{SHOW_HIDDEN_FOLDERS}:{profile_id}"))
            .inspect_err(|err| tracing::warn!(profile_id, %err, "failed to read whether storage shows hidden folders"))
            .ok()
            .flatten()
            .as_deref()
            == Some("1")
    });
    let is_lone = roots.len() < 2;
    let mut scan = Scan {
        folders: Vec::with_capacity(roots.len()),
        left_out: Vec::new(),
        hidden: roots.iter().filter(|(root, _)| hidden_roots.contains(root)).count(),
        show_hidden,
    };
    for (root, is_primary) in roots {
        let is_default = default.as_ref().map_or(is_primary, |default| *default == root);
        let hidden = hidden_roots.contains(&root);
        if hidden && !show_hidden && !is_default && !is_lone {
            scan.left_out.push(root);
            continue;
        }
        let mut folder = Folder::scan(root, is_primary, &owners, db.as_ref());
        folder.is_default = is_default;
        folder.hidden = hidden;
        scan.folders.push(folder);
    }
    tracing::debug!(
        profile_id,
        folders = scan.folders.len(),
        left_out = scan.left_out.len(),
        "computed storage usage"
    );
    scan
}

/// What a folder's card says about its disk: games, free space, or that the disk is away.
fn folder_status(folder: &Folder) -> String {
    let size = crate::format::bytes(folder.total);
    let count = folder.games.len() + folder.dumps.len();
    let used = if count == 1 {
        t!("storage.folder.used.one", used = size)
    } else {
        t!("storage.folder.used.other", used = size, count = count)
    };
    match (folder.present, folder.disk) {
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
    }
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
    /// The games and dumps picked with their checkboxes to move together, all from `chosen`.
    selected: HashSet<MoveKey>,
    /// The library folder whose games are listed; the primary one until another is picked.
    chosen: Option<PathBuf>,
    /// The folder decanter kept everything in before it moved into the
    /// library, while it's still there.
    legacy: Option<PathBuf>,
    confirm_legacy: bool,
    /// The journal-less staging folder whose Eliminar waits for its
    /// confirmation: library folder, account and folder name.
    confirm_stray: Option<(PathBuf, String, String)>,
    /// Library folders scanned for games decanter didn't install: `None` while the scan runs,
    /// then how many it found.
    unmanaged_scans: HashMap<PathBuf, Option<usize>>,
    /// Hidden folders the last scan left out of `folders`.
    left_out: Vec<PathBuf>,
    /// How many folders are hidden; the "Show hidden libraries" check is
    /// offered only while there is one.
    hidden_folders: usize,
    show_hidden: bool,
}

impl StorageView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state = AppState::get(cx).clone();
        cx.observe(&state.session, |this, _, cx| this.rescan(cx)).detach();
        cx.observe(&state.background, |_, _, cx| cx.notify()).detach();
        cx.observe(&state.moves, |_, _, cx| cx.notify()).detach();
        // A game moved into or out of a hidden folder changes what the Library hides.
        cx.subscribe(&state.moves, |this, _, _: &MoveFinished, cx| {
            crate::views::library::actions::bump_organisation(cx);
            this.rescan(cx);
        })
        .detach();
        let mut this = Self {
            folders: None,
            mounts: Vec::new(),
            error: None,
            scan: None,
            queue: state.moves.clone(),
            selected: HashSet::new(),
            chosen: None,
            legacy: None,
            confirm_legacy: false,
            confirm_stray: None,
            unmanaged_scans: HashMap::new(),
            left_out: Vec::new(),
            hidden_folders: 0,
            show_hidden: false,
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
        // Steam's library folders first, so a card mounted since the last visit lists with the rest.
        let scan = cx.background_spawn(async move {
            let is_changed = crate::steam_compat::sync(Some(&profile_id));
            (scan(&profile_id), decanter_core::removable_mounts(), is_changed)
        });
        self.scan = Some(cx.spawn(async move |this, cx| {
            let (scan, mounts, is_changed) = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.folders = Some(scan.folders);
                this.left_out = scan.left_out;
                this.hidden_folders = scan.hidden;
                this.show_hidden = scan.show_hidden;
                this.mounts = mounts;
                if is_changed {
                    AppState::get(cx)
                        .library
                        .clone()
                        .update(cx, |library, cx| library.refresh_all_install_state(cx));
                }
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
        crate::views::library::actions::bump_organisation(cx);
        self.rescan(cx);
    }

    /// Hides `root`'s games from the Library views, or shows them again; the
    /// Library re-reads its hidden set from the flag stored here.
    fn set_folder_hidden(&mut self, root: &Path, hidden: bool, cx: &mut Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return;
        };
        let stored = crate::core::db().map_err(|err| err.to_string()).and_then(|db| {
            db.set_folder_hidden(&profile_id, root, hidden)
                .map_err(|err| err.to_string())
        });
        match stored {
            Ok(()) => {
                self.error = None;
                if let Some(folder) = self.folders.iter_mut().flatten().find(|folder| folder.root == root) {
                    folder.hidden = hidden;
                }
                crate::views::library::actions::bump_organisation(cx);
                // The count behind "Show hidden libraries" moved, and a folder hidden while hidden
                // ones aren't shown leaves the page.
                self.rescan(cx);
            }
            Err(err) => {
                tracing::warn!(profile_id, root = %root.display(), hidden, %err, "could not change the folder's hidden state");
                self.error = Some(Text::new("storage.errors.hide").arg("error", err));
            }
        }
        cx.notify();
    }

    /// Whether the profile has a single library folder, counting the hidden ones left out.
    fn is_lone(&self) -> bool {
        self.folders
            .as_ref()
            .map_or(0, Vec::len)
            .saturating_add(self.left_out.len())
            < 2
    }

    fn set_show_hidden(&mut self, shown: bool, cx: &mut Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return;
        };
        let value = if shown { "1" } else { "0" };
        if let Err(err) = crate::core::db().map_err(|err| err.to_string()).and_then(|db| {
            db.set_setting(&format!("{SHOW_HIDDEN_FOLDERS}:{profile_id}"), value)
                .map_err(|err| err.to_string())
        }) {
            tracing::warn!(profile_id, shown, %err, "failed to save whether storage shows hidden folders");
        }
        self.show_hidden = shown;
        self.rescan(cx);
    }

    /// "Show hidden libraries", above the folder list while a folder is hidden.
    fn show_hidden_toggle(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let shown = self.show_hidden;
        (self.hidden_folders > 0).then(|| {
            crate::widgets::nav::tab_stop(div().id("storage-show-hidden"))
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.set_show_hidden(!shown, cx)))
                .child(checkbox(shown))
                .child(theme::muted(t!("storage.show_hidden")).text_size(px(12.)))
        })
    }

    /// The "Hide library" check of a folder's card: small and muted, since it only changes what the
    /// Library shows, and it doesn't pick the card it sits on. The default install target and a lone
    /// folder can't be hidden, since new installs land in them; one hidden before can be shown.
    fn hide_toggle(index: usize, folder: &Folder, is_lone: bool, cx: &Context<Self>) -> impl IntoElement + use<> {
        let (root, hidden) = (folder.root.clone(), folder.hidden);
        let is_blocked = !hidden && (folder.is_default || is_lone);
        let toggle = div()
            .id(("storage-hide", index))
            .flex()
            .items_center()
            .gap_1()
            .tooltip(tooltip(if is_blocked {
                t!("storage.folder.hide_blocked")
            } else {
                t!("storage.folder.hide_hint")
            }))
            .child(checkbox(hidden).when(is_blocked, |b| b.opacity(0.4)))
            .child(theme::muted(t!("storage.folder.hide")).text_size(px(12.)));
        if is_blocked {
            return toggle.into_any_element();
        }
        crate::widgets::nav::tab_stop(toggle.cursor_pointer())
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.set_folder_hidden(&root, !hidden, cx);
            }))
            .into_any_element()
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
                    .map(|folder| &folder.root)
                    .chain(&self.left_out)
                    .any(|root| root.starts_with(mount) || mount.starts_with(root))
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

    /// Looks through `root` for games decanter didn't install, off the UI thread, for External games
    /// to propose.
    fn scan_unmanaged(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return;
        };
        self.unmanaged_scans.insert(root.to_path_buf(), None);
        cx.notify();
        let root = root.to_path_buf();
        let scan = cx.background_spawn({
            let (profile_id, root) = (profile_id.clone(), root.clone());
            async move { crate::external::unmanaged::detect(&profile_id, &root) }
        });
        cx.spawn(async move |this, cx| {
            let found = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.unmanaged_scans
                    .insert(root.clone(), Some(found.games.len().saturating_add(found.more)));
                crate::external::unmanaged::store(&profile_id, root, found, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn scan_button(&self, index: usize, root: &Path, cx: &Context<Self>) -> impl IntoElement + use<> {
        let state = self.unmanaged_scans.get(root).copied();
        let scanning = state == Some(None);
        let label = if scanning {
            t!("storage.folder.scan.running")
        } else {
            t!("storage.folder.scan.button")
        };
        let result = match state.flatten() {
            Some(0) => Some(t!("storage.folder.scan.none")),
            Some(count) => Some(t!("storage.folder.scan.found", count = count)),
            None => None,
        };
        let root = root.to_path_buf();
        div()
            .flex()
            .items_center()
            .gap_2()
            .children(result.map(|result| theme::muted(result).text_size(px(12.))))
            .child(
                button(("storage-scan", index), ButtonKind::Secondary, label, scanning)
                    .tooltip(tooltip(t!("storage.folder.scan.hint")))
                    .when(!scanning, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| this.scan_unmanaged(&root, cx)))
                    }),
            )
    }

    fn use_as_default(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(profile_id) = AppState::get(cx).session.read(cx).active_profile.clone() else {
            return;
        };
        match crate::core::set_default_install_folder(&profile_id, root) {
            Ok(()) => {
                self.error = None;
                self.rescan(cx);
            }
            Err(err) => self.error = Some(err),
        }
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

    /// One library folder in the list on the left: which disk it is, how full, and whether it is
    /// the one whose games the panel on the right lists.
    fn library_pick(
        &self,
        index: usize,
        folder: &Folder,
        chosen: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement + use<> {
        let label = if folder.primary {
            t!("storage.folder.primary")
        } else {
            t!("storage.folder.extra")
        };
        let used = folder
            .disk
            .map(|(free, total)| (total.saturating_sub(free)) as f64 / total.max(1) as f64);
        let root = folder.root.clone();
        reveal(
            crate::widgets::nav::tab_stop(div().id(("storage-pick", index)))
                .flex()
                .flex_col()
                .gap_2()
                .p(px(14.))
                .rounded(px(theme::RADIUS))
                .border_1()
                .border_color(rgb(if chosen { theme::ACCENT_BLUE } else { theme::LINE }))
                .bg(rgb(if chosen { theme::BG_PANEL } else { theme::BG_PANEL_2 }))
                .cursor_pointer()
                .when(!folder.present, |card| card.opacity(0.6))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.chosen.as_ref() != Some(&root) {
                        this.chosen = Some(root.clone());
                        this.selected.clear();
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(theme::badge(label, folder.primary))
                        .when(folder.is_default, |row| {
                            row.child(theme::badge(t!("storage.folder.default"), true))
                        })
                        .when(!folder.present, |row| {
                            row.child(theme::chip(t!("storage.folder.offline_chip"), theme::Tone::Warn))
                        })
                        .when(folder.hidden, |row| {
                            row.child(theme::chip(t!("storage.folder.hidden_chip"), theme::Tone::Neutral))
                        }),
                )
                .child(
                    theme::mono()
                        .text_size(px(11.))
                        .text_color(rgb(theme::TEXT_PRIMARY))
                        .truncate()
                        .child(folder.root.display().to_string()),
                )
                .children(used.map(|used| progress_bar(("storage-pick-bar", index), Some(used), 6.)))
                .child(theme::muted(folder_status(folder)).text_size(px(12.)))
                .child(Self::hide_toggle(index, folder, self.is_lone(), cx)),
        )
    }

    fn folder_card(&self, index: usize, folder: &Folder, cx: &Context<Self>) -> impl IntoElement + use<> {
        let label = if folder.primary {
            t!("storage.folder.primary")
        } else {
            t!("storage.folder.extra")
        };
        let count = folder.games.len() + folder.dumps.len();
        let status = folder_status(folder);
        // A folder with games still in it would take them out of the
        // library without moving them anywhere. An unplugged one can't be
        // checked, and is the folder most likely to be gone for good.
        let removable = !folder.primary && (count == 0 || !folder.present);
        let root = folder.root.clone();
        let default_root = folder.root.clone();
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
                    .when(folder.is_default, |row| {
                        row.child(theme::badge(t!("storage.folder.default"), true))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(folder.root.display().to_string())
                            .child(theme::muted(status).text_size(px(12.)))
                            .when(folder.steam_games > 0, |col| {
                                let steam_size = crate::format::bytes(folder.steam_size);
                                let steam = if folder.steam_games == 1 {
                                    t!("storage.folder.steam.one", size = steam_size)
                                } else {
                                    t!(
                                        "storage.folder.steam.other",
                                        size = steam_size,
                                        count = folder.steam_games
                                    )
                                };
                                col.child(theme::muted(steam).text_size(px(12.)))
                            }),
                    )
                    .when(folder.present, |row| {
                        row.child(self.scan_button(index, &folder.root, cx))
                    })
                    .when(!folder.is_default && folder.present, |row| {
                        row.child(
                            button(
                                ("storage-default", index),
                                ButtonKind::Secondary,
                                t!("storage.folder.use_default"),
                                false,
                            )
                            .tooltip(tooltip(t!("storage.folder.use_default_hint")))
                            .on_click(cx.listener(move |this, _, _, cx| this.use_as_default(&default_root, cx))),
                        )
                    })
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
            .child(Self::cache_row(index, folder, cx))
            .children(self.unfinished_rows(index, folder, cx))
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
        let chosen = self
            .chosen
            .as_ref()
            .and_then(|root| folders.iter().position(|folder| folder.root == *root))
            .unwrap_or(0);
        let (games, used) = folders.iter().fold((0, 0), |(games, used), folder| {
            (games + folder.games.len() + folder.dumps.len(), used + folder.total)
        });

        let (steam_games, steam_size) = folders.iter().fold((0, 0), |(games, size), folder| {
            (games + folder.steam_games, size + folder.steam_size)
        });
        let summary = if steam_games == 0 {
            t!("storage.summary.own", used = crate::format::bytes(used), count = games)
        } else {
            t!(
                "storage.summary.with_steam",
                used = crate::format::bytes(used),
                count = games,
                steam = crate::format::bytes(steam_size),
                steam_count = steam_games
            )
        };
        let stats = theme::muted(summary).mb_6();

        theme::page(
            "storage",
            column
                .child(stats)
                .children(self.selection_bar(cx))
                .children(self.queue_card(cx))
                .child(
                    div()
                        .flex()
                        .items_start()
                        .gap_4()
                        .child(
                            div()
                                .w(px(300.))
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .children(self.show_hidden_toggle(cx))
                                .children(
                                    folders
                                        .iter()
                                        .enumerate()
                                        .map(|(index, folder)| self.library_pick(index, folder, index == chosen, cx)),
                                )
                                .child(reveal(
                                    div()
                                        .id("storage-add")
                                        .flex()
                                        .flex_col()
                                        .items_start()
                                        .gap_2()
                                        .child(
                                            button(
                                                "storage-add-folder",
                                                ButtonKind::Secondary,
                                                t!("storage.add"),
                                                false,
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                                        )
                                        .children(self.mount_suggestions(folders, cx)),
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .children(folders.get(chosen).map(|folder| self.folder_card(chosen, folder, cx))),
                        ),
                )
                .children(self.error.as_ref().map(|error| div().mt_3().child(theme::error(error))))
                .children(self.legacy.as_deref().map(|legacy| self.legacy_card(legacy, cx))),
        )
    }
}
