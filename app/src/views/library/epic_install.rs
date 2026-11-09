//! Epic Games titles through the same Descargas rows, pause, cancel and properties as GOG's and
//! Amazon's (kb #1619); the download itself is `decanter_epic`'s.

use decanter_core::{CachedGame, InstalledGame, Library, Storefront};
use gpui::AsyncApp;

use super::install::{DownloadJob, Ended, Imported, progress_channel, refresh_install_state, track};
use crate::i18n::Text;
use crate::state::{AppState, DownloadKey, DownloadKind, InstallStatus};

/// Whether `account_id` is one of the profile's Epic Games accounts, which every download,
/// uninstall and verify of its games goes through `decanter_epic` for.
pub fn is_epic_account(account_id: &str) -> bool {
    crate::core::db()
        .ok()
        .and_then(|db| {
            db.profile_id_for_account(account_id, Storefront::EpicGames)
                .inspect_err(|err| tracing::warn!(account_id, %err, "failed to look up an epic account"))
                .ok()
        })
        .flatten()
        .is_some()
}

/// The sandbox and catalog item the library listing cached for the game, which Epic's manifest
/// request names beside its `appName`.
struct Ids {
    namespace: String,
    catalog_item_id: String,
}

/// What the library listing cached for the game: the ids its manifest request and launch name, and
/// the catalog attributes its launch reads.
pub fn cached_game(account_id: &str, app_name: &str) -> Option<CachedGame> {
    crate::core::db()
        .ok()?
        .list_games(account_id)
        .inspect_err(|err| tracing::warn!(account_id, %err, "failed to read the cached epic games"))
        .ok()?
        .into_iter()
        .find(|game| game.storefront == Storefront::EpicGames && game.product_id == app_name)
}

/// Whether the catalog said the game does not start without Epic's online session, which the
/// offline launch does not give it.
pub fn needs_online(account_id: &str, app_name: &str) -> bool {
    cached_game(account_id, app_name).and_then(|game| game.can_run_offline) == Some(false)
}

/// Whether the record's program is a regular file inside the game's folder, reached through no
/// symbolic link, as the install checked it: a link put there since could start something outside.
pub fn is_program_inside(account_id: &str, game: &InstalledGame) -> bool {
    let product_id = game.game_id.as_str();
    let Some(program) = game.launch_executable.as_deref() else {
        return false;
    };
    match program_file_type(account_id, game, program) {
        Ok(file_type) if file_type.is_file() => true,
        Ok(_) => {
            tracing::warn!(
                account_id,
                product_id,
                program,
                "the epic game's program is not a regular file, not launched"
            );
            false
        }
        Err(reason) => {
            tracing::warn!(
                account_id,
                product_id,
                program,
                reason,
                "the epic game's program is refused, not launched"
            );
            false
        }
    }
}

fn program_file_type(account_id: &str, game: &InstalledGame, program: &str) -> Result<std::fs::FileType, String> {
    let library = crate::core::library_for_account(account_id).map_err(|err| err.to_string())?;
    let folder = library
        .install_dir(&game.install_dir)
        .ok_or_else(|| format!("unusable install folder {:?}", game.install_dir))?;
    let path = decanter_runtime::install::ManifestPath::parse(program).map_err(|err| err.to_string())?;
    let full = decanter_runtime::install::files::inside(&folder, &path).map_err(|err| err.to_string())?;
    std::fs::symlink_metadata(full)
        .map(|meta| meta.file_type())
        .map_err(|err| err.to_string())
}

fn cached_ids(account_id: &str, app_name: &str) -> Option<Ids> {
    let game = cached_game(account_id, app_name)?;
    Some(Ids {
        namespace: game.namespace?,
        catalog_item_id: game.catalog_item_id?,
    })
}

fn fail_row(state: &AppState, key: &DownloadKey, title: &str, kind: DownloadKind, text: Text, cx: &mut AsyncApp) {
    state.background.update(cx, |background, cx| {
        let row = background
            .installing
            .entry(key.clone())
            .or_insert_with(|| super::install::queued(title.to_owned(), kind));
        row.status = InstallStatus::Failed(text);
        cx.notify();
    });
}

/// Installs, carries on, updates or repairs the game under `key` as a tracked row, a DLC into the
/// game the library lists it under; whether it finished. `library_root` picks the library folder of
/// a new install; an existing one, and a DLC, stay where the game is.
pub async fn download(
    key: DownloadKey,
    title: String,
    kind: DownloadKind,
    library_root: Option<std::path::PathBuf>,
    state: &AppState,
    cx: &mut AsyncApp,
) -> bool {
    let DownloadKey { account_id, product_id } = &key;
    let is_repair = kind == DownloadKind::Repair;
    tracing::info!(account_id, product_id, title, is_repair, "epic download requested");
    let cached = cached_game(account_id, product_id);
    // The listing, not the row's kind, says whether it is a DLC: a download listed again after a
    // restart from an older staging record carries no parent.
    let parent_id = cached.as_ref().and_then(|game| game.parent_id.clone());
    let Some(ids) = cached.and_then(|game| {
        Some(Ids {
            namespace: game.namespace?,
            catalog_item_id: game.catalog_item_id?,
        })
    }) else {
        tracing::error!(
            account_id,
            product_id,
            "no cached namespace or catalog item, the epic download can't start"
        );
        fail_row(state, &key, &title, kind, Text::new("epic.install.no_ids"), cx);
        return false;
    };
    if let Some(parent_id) = parent_id.as_deref()
        && crate::core::installed_game(account_id, parent_id).is_none()
    {
        tracing::warn!(
            account_id,
            product_id,
            parent_id,
            "the epic dlc's game is not installed, the download can't start"
        );
        fail_row(state, &key, &title, kind, Text::new("epic.install.needs_game"), cx);
        return false;
    }
    let library = match crate::core::library_for_account(account_id) {
        Ok(library) => match library_root.as_deref().filter(|_| parent_id.is_none()) {
            Some(root) => library.targeting(root),
            None => library,
        },
        Err(err) => {
            tracing::error!(account_id, product_id, %err, "could not resolve the library, epic download aborted");
            fail_row(state, &key, &title, kind, Text::new("library.install.no_library"), cx);
            return false;
        }
    };
    let dlcs = match (is_repair, parent_id.as_deref()) {
        (true, None) => installed_dlcs(account_id, product_id),
        _ => Vec::new(),
    };
    let (on_progress, progress) = progress_channel();
    let mut target = decanter_epic::Target::new(account_id, product_id, &ids.namespace, &ids.catalog_item_id, &title);
    if let Some(parent_id) = parent_id.as_deref() {
        target = target.dlc_of(parent_id);
    }
    let dlc_targets: Vec<_> = dlcs.iter().map(|dlc| dlc.target(account_id, product_id)).collect();
    let work = async {
        if is_repair {
            decanter_epic::repair(&library, target, &dlc_targets, on_progress).await
        } else {
            decanter_epic::install(&library, target, on_progress).await
        }
    };
    let job = DownloadJob {
        key: key.clone(),
        title: title.clone(),
        kind,
    };
    let is_done = matches!(track(state, job, progress, work, cx).await, Ended::Done(()));
    if is_done {
        tracing::info!(account_id, product_id, is_repair, "epic download finished");
    }
    refresh_install_state(state, account_id, product_id, cx);
    if let Some(parent_id) = parent_id.as_deref() {
        refresh_install_state(state, account_id, parent_id, cx);
    }
    is_done
}

/// Installs, one after the other into the game just installed, the DLC picked with it that have a
/// build of their own; each reports on its own row, and one that fails leaves the game as it is.
pub async fn install_dlcs(
    account_id: &str,
    product_id: &str,
    dlcs: Vec<(String, String)>,
    state: &AppState,
    cx: &mut AsyncApp,
) {
    for (dlc_id, dlc_title) in dlcs {
        if cached_game(account_id, &dlc_id).is_none_or(|dlc| !dlc.installable) {
            tracing::info!(
                account_id,
                product_id,
                dlc_id,
                "epic dlc without a build of its own, not installed"
            );
            continue;
        }
        tracing::info!(account_id, product_id, dlc_id, "epic dlc install started");
        download(
            DownloadKey::new(account_id, &dlc_id),
            dlc_title,
            DownloadKind::Dlc,
            None,
            state,
            cx,
        )
        .await;
    }
}

/// An installed DLC of a game, with the ids its repair asks Epic by.
struct InstalledDlc {
    app_name: String,
    title: String,
    ids: Ids,
}

impl InstalledDlc {
    fn target<'dlc>(&'dlc self, account_id: &'dlc str, parent_id: &'dlc str) -> decanter_epic::Target<'dlc> {
        decanter_epic::Target::new(
            account_id,
            &self.app_name,
            &self.ids.namespace,
            &self.ids.catalog_item_id,
            &self.title,
        )
        .dlc_of(parent_id)
    }
}

/// The DLC recorded on the installed game; one the cache names no ids for is left out of a repair,
/// and logged.
fn installed_dlcs(account_id: &str, product_id: &str) -> Vec<InstalledDlc> {
    let Some(game) = crate::core::installed_game(account_id, product_id) else {
        return Vec::new();
    };
    game.dlcs
        .into_iter()
        .filter_map(|(app_name, dlc)| {
            let Some(ids) = cached_ids(account_id, &app_name) else {
                tracing::warn!(
                    account_id,
                    product_id,
                    dlc_id = app_name,
                    "no cached ids for an installed epic dlc, left out of the repair"
                );
                return None;
            };
            Some(InstalledDlc {
                app_name,
                title: dlc.title,
                ids,
            })
        })
        .collect()
}

/// Registers `folder` as the installed game, or says how far it is from Epic's files.
pub async fn import(
    key: &DownloadKey,
    title: &str,
    folder: &std::path::Path,
    differences: decanter_epic::Differences,
    state: &AppState,
    cx: &mut AsyncApp,
) -> Result<Imported, Text> {
    let DownloadKey { account_id, product_id } = key;
    let ids = cached_ids(account_id, product_id).ok_or_else(|| {
        tracing::error!(
            account_id,
            product_id,
            "no cached namespace or catalog item, the epic import can't start"
        );
        Text::new("epic.install.no_ids")
    })?;
    let library = crate::core::library_for_account(account_id)
        .map_err(|err| Text::new("library.install.library_error").text("error", err.localized()))?;
    let target = decanter_epic::Target::new(account_id, product_id, &ids.namespace, &ids.catalog_item_id, title);
    let found = decanter_epic::import(&library, target, folder, differences)
        .await
        .inspect_err(|err| {
            tracing::error!(account_id, product_id, error = %format_args!("{err:#}"), "epic import failed");
        })
        .map_err(|err| Text::verbatim(format!("{err:#}")))?;
    if found.is_recorded {
        refresh_install_state(state, account_id, product_id, cx);
    }
    Ok(Imported::from(&found))
}

/// Deletes the stopped download of `key`'s game, the confirmed cancel of a paused row.
pub async fn discard(library: &Library, key: &DownloadKey) -> Result<(), Text> {
    decanter_epic::discard(library, &key.account_id, &key.product_id)
        .await
        .inspect_err(|err| {
            tracing::error!(
                account_id = key.account_id,
                product_id = key.product_id,
                error = %format_args!("{err:#}"),
                "deleting a paused epic download failed"
            );
        })
        .map_err(Text::verbatim)
}

/// Uninstalls the game with the DLC installed into it, and the paused downloads of both, which the
/// confirmation named.
pub async fn uninstall(account_id: &str, product_id: &str, state: &AppState, cx: &mut AsyncApp) -> Result<(), Text> {
    tracing::info!(account_id, product_id, "epic uninstall started");
    let library = crate::core::library_for_account(account_id)
        .map_err(|err| Text::new("library.install.library_error").text("error", err.localized()))?;
    let dlc_ids: Vec<String> = state.library.read_with(cx, |library, _| {
        library
            .games
            .iter()
            .filter(|entry| entry.parent_id.as_deref() == Some(product_id))
            .map(|entry| entry.game.id.clone())
            .collect()
    });
    for dlc_id in &dlc_ids {
        decanter_epic::discard(&library, account_id, dlc_id)
            .await
            .inspect_err(|err| {
                tracing::error!(account_id, dlc_id, error = %format_args!("{err:#}"), "deleting a paused epic dlc download failed");
            })
            .map_err(Text::verbatim)?;
    }
    decanter_epic::uninstall(&library, account_id, product_id)
        .await
        .inspect_err(|err| {
            tracing::error!(account_id, product_id, error = %format_args!("{err:#}"), "epic uninstall failed");
        })
        .map_err(Text::verbatim)?;
    drop_paused_rows(
        state,
        account_id,
        dlc_ids.iter().map(String::as_str).chain([product_id]),
        cx,
    );
    for dlc_id in &dlc_ids {
        refresh_install_state(state, account_id, dlc_id, cx);
    }
    refresh_install_state(state, account_id, product_id, cx);
    tracing::info!(account_id, product_id, dlcs = dlc_ids.len(), "epic uninstall finished");
    Ok(())
}

/// Uninstalls one DLC from its game, and its paused download with it.
pub async fn uninstall_dlc(
    account_id: &str,
    dlc_id: &str,
    parent_id: &str,
    state: &AppState,
    cx: &mut AsyncApp,
) -> Result<(), Text> {
    tracing::info!(account_id, dlc_id, parent_id, "epic dlc uninstall started");
    let library = crate::core::library_for_account(account_id)
        .map_err(|err| Text::new("library.install.library_error").text("error", err.localized()))?;
    decanter_epic::uninstall_dlc(&library, account_id, dlc_id, parent_id)
        .await
        .inspect_err(|err| {
            tracing::error!(account_id, dlc_id, parent_id, error = %format_args!("{err:#}"), "epic dlc uninstall failed");
        })
        .map_err(Text::verbatim)?;
    drop_paused_rows(state, account_id, [dlc_id], cx);
    refresh_install_state(state, account_id, dlc_id, cx);
    refresh_install_state(state, account_id, parent_id, cx);
    tracing::info!(account_id, dlc_id, parent_id, "epic dlc uninstall finished");
    Ok(())
}

/// Drops the paused rows of downloads an uninstall deleted.
fn drop_paused_rows<'id>(
    state: &AppState,
    account_id: &str,
    product_ids: impl IntoIterator<Item = &'id str>,
    cx: &mut AsyncApp,
) {
    state.background.update(cx, |background, cx| {
        for product_id in product_ids {
            let key = DownloadKey::new(account_id, product_id);
            if background
                .installing
                .get(&key)
                .is_some_and(|row| matches!(row.status, InstallStatus::Paused { .. }))
            {
                background.installing.remove(&key);
            }
        }
        cx.notify();
    });
}

/// Verifies the game against the manifest it was installed from, in the shape Archivos locales
/// shows for every store.
pub async fn verify(
    library: &Library,
    account_id: &str,
    product_id: &str,
) -> anyhow::Result<decanter_gog::VerifyReport> {
    let report = decanter_epic::verify(library, account_id, product_id).await?;
    let broken = report
        .broken
        .into_iter()
        .map(|file| decanter_gog::BrokenFile {
            relative_path: file.path,
            owner: product_id.to_owned(),
            reason: match file.reason {
                decanter_epic::BrokenReason::Missing => decanter_gog::BrokenReason::Missing,
                // A kind this build does not know is still a file that needs fetching again.
                _ => decanter_gog::BrokenReason::Mismatched,
            },
        })
        .collect();
    Ok(decanter_gog::VerifyReport {
        files_checked: report.files_checked,
        broken,
    })
}

/// Fetches again what is missing or wrong in the installed game and its DLC, as a tracked row's work.
pub async fn repair(
    library: &Library,
    account_id: &str,
    product_id: &str,
    title: &str,
    on_progress: impl FnMut(decanter_gog::Progress) + Send + 'static,
) -> anyhow::Result<()> {
    let ids = cached_ids(account_id, product_id)
        .ok_or_else(|| anyhow::anyhow!("no Epic namespace or catalog item is cached for this game yet"))?;
    let target = decanter_epic::Target::new(account_id, product_id, &ids.namespace, &ids.catalog_item_id, title);
    let dlcs = installed_dlcs(account_id, product_id);
    let dlc_targets: Vec<_> = dlcs.iter().map(|dlc| dlc.target(account_id, product_id)).collect();
    decanter_epic::repair(library, target, &dlc_targets, on_progress).await
}
