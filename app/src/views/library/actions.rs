//! What the detail pane's buttons and the right-click menu both do to a
//! game: launch it, install it, find its files. Each long-running action
//! is spawned on the app, detached — it outlives whichever button started
//! it, since that button is swapped out the moment the action's own state
//! (running, installing) changes.

use std::borrow::Cow;

use decanter_core::{Game, InstalledGame, platform};
use gpui::App;
use rust_i18n::t;

use super::data;
use super::install::{self, InstallRequest};
use crate::state::{AppState, DownloadKey, DownloadKind, InstallStatus};

/// Why `game` can't be installed or played here, when this system has no
/// build of it: shown in place of Instalar and Jugar.
pub fn missing_build(game: &Game) -> Option<Cow<'static, str>> {
    let system = platform::game_system();
    (!game.has_build_for(system)).then(|| t!("library.detail.no_build", system = system.to_string()))
}

/// The installed game behind a catalog entry, or `None` if this profile
/// doesn't have it. One manifest read — the manifest names the files, the
/// build and what launches it.
pub fn resolve_launch_target(account_id: &str, game: &Game) -> Option<InstalledGame> {
    let product_id = game.id.as_str();
    let Some(installed) = crate::core::installed_game(account_id, product_id) else {
        tracing::debug!(account_id, product_id, "no install manifest found, not launchable");
        return None;
    };
    // No executable means installed-but-not-launchable, which the caller
    // renders differently from not installed at all.
    let Some(_) = installed.launch_executable.as_ref() else {
        tracing::warn!(account_id, product_id, "installed but no launch executable resolved");
        return None;
    };
    Some(installed)
}

/// Where this game's files live on disk, once it's installed. Unlike
/// [`resolve_launch_target`], this doesn't need a launch executable — it
/// works for the "instalado, pero sin ejecutable detectado" case too,
/// where there's nothing to start but the files are still there to look at.
pub fn game_install_path(account_id: &str, product_id: &str) -> Option<std::path::PathBuf> {
    let Some(installed) = crate::core::installed_game(account_id, product_id) else {
        tracing::debug!(
            account_id,
            product_id,
            "no install manifest found, no install path to show"
        );
        return None;
    };
    // Never installed under `common/`: its files are wherever the
    // executable it was pointed at lives.
    if installed.storefront == decanter_core::Storefront::Unspecified {
        return crate::core::executable_for(&installed).and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
    }
    let Some(library) = crate::core::library_for_account(account_id)
        .inspect_err(|err| tracing::warn!(account_id, product_id, error = %err, "failed to resolve library root"))
        .ok()
    else {
        return None;
    };
    library.install_dir(&installed.install_dir)
}

/// Hands a path to whatever the desktop's own file manager is.
pub fn open_in_file_manager(path: &std::path::Path) {
    if let Err(err) = crate::desktop::open(path) {
        tracing::warn!(path = %path.display(), %err, "could not open in the file manager");
    }
}

/// The launch shared by the detail pane's PLAY button and the right-click
/// menu's. The session bookkeeping lives in `crate::launch`; what's here
/// on top of it is the playtime tally.
pub fn launch_game(account_id: String, game: InstalledGame, cx: &mut App) {
    let background = AppState::get(cx).background.clone();
    cx.spawn(async move |cx| {
        let product_id = game.game_id.clone();
        let storefront = game.storefront;
        tracing::info!(account_id, product_id, "launching game");
        let Some(played) = crate::launch::start(account_id.clone(), game, background.clone(), cx).await else {
            return;
        };
        tracing::debug!(
            account_id,
            product_id,
            seconds = played.as_secs(),
            "recording play session"
        );
        data::add_playtime(&account_id, storefront, &product_id, played.as_secs() as i64);
        background.update(cx, |background, cx| {
            background.playtime_generation = background.playtime_generation.wrapping_add(1);
            cx.notify();
        });
    })
    .detach();
}

pub fn stop_game(account_id: String, game_id: String, cx: &mut App) {
    let background = AppState::get(cx).background.clone();
    cx.spawn(async move |cx| crate::launch::stop(account_id, game_id, background, cx).await)
        .detach();
}

/// Queues the install and starts it. `branch` pins the base game to a
/// specific GOG branch (a beta, say) — ignored for a DLC, whose branch
/// always comes from the base game it belongs to.
pub fn start_install(request: InstallRequest, cx: &mut App) {
    let state = AppState::get(cx).clone();
    let key = DownloadKey::new(&request.account_id, &request.product_id);
    let kind = if request.parent_id.is_some() {
        DownloadKind::Dlc
    } else {
        DownloadKind::Install
    };
    // On screen at once, so Instalar can't be pressed twice before the
    // task below gets to run.
    let row = install::queued(request.title.clone(), kind);
    state.background.update(cx, |background, cx| {
        background.installing.insert(key, row);
        cx.notify();
    });
    cx.spawn(async move |cx| install::install(request, state, cx).await)
        .detach();
}

/// Cancelar only asks: confirming deletes what the download had already
/// fetched (#1133), and the question shows how much that is.
pub fn ask_cancel(key: DownloadKey, cx: &mut App) {
    let background = AppState::get(cx).background.clone();
    background.update(cx, |background, cx| {
        background.cancel_confirm = Some(key);
        cx.notify();
    });
}

pub fn keep_download(cx: &mut App) {
    let background = AppState::get(cx).background.clone();
    background.update(cx, |background, cx| {
        background.cancel_confirm = None;
        cx.notify();
    });
}

/// The confirmed cancel. The row stays as Cancelling until the download
/// has stopped and its staging is gone; its own task then removes it, or,
/// for a paused one nothing runs, the discard does.
pub fn confirm_cancel(key: DownloadKey, cx: &mut App) {
    let state = AppState::get(cx).clone();
    let was_paused = state.background.update(cx, |background, cx| {
        background.cancel_confirm = None;
        let mut was_paused = false;
        if let Some(row) = background.installing.get_mut(&key) {
            was_paused = matches!(row.status, InstallStatus::Paused { .. });
            row.status = InstallStatus::Cancelling {
                bytes_downloaded: row.status.bytes_downloaded(),
            };
        }
        cx.notify();
        was_paused
    });
    tracing::info!(
        account_id = key.account_id,
        product_id = key.product_id,
        was_paused,
        "download cancel confirmed"
    );
    if was_paused {
        cx.spawn(async move |cx| install::discard_paused(key, state, cx).await)
            .detach();
    } else {
        cx.spawn(async move |_| decanter_gog::cancel(&key.account_id, &key.product_id).await)
            .detach();
    }
}

/// Drops a failed download's notice.
pub fn dismiss_install(key: &DownloadKey, cx: &mut App) {
    let background = AppState::get(cx).background.clone();
    background.update(cx, |background, cx| {
        background.installing.remove(key);
        cx.notify();
    });
}

pub fn bump_organisation(cx: &mut App) {
    let background = AppState::get(cx).background.clone();
    background.update(cx, |background, cx| background.bump_organisation(cx));
}
