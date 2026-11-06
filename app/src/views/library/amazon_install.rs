//! Amazon Games titles through the same Descargas rows, pause, cancel and properties as GOG's
//! (kb #1613); the download itself is `decanter_amazon`'s. No launch yet: that is stage S5.

use decanter_core::{Library, Storefront};
use gpui::AsyncApp;

use super::install::{DownloadJob, Ended, progress_channel, refresh_install_state, track};
use crate::i18n::Text;
use crate::state::{AppState, DownloadKey, DownloadKind, InstallStatus};

/// Whether `account_id` is one of the profile's Amazon Games accounts, which every download,
/// uninstall and verify of its games goes through `decanter_amazon` for.
pub fn is_amazon_account(account_id: &str) -> bool {
    crate::core::db()
        .ok()
        .and_then(|db| {
            db.profile_id_for_account(account_id, Storefront::AmazonGames)
                .inspect_err(|err| tracing::warn!(account_id, %err, "failed to look up an amazon account"))
                .ok()
        })
        .flatten()
        .is_some()
}

/// The entitlement id the library listing cached beside the product, which Amazon's download
/// request takes instead of the product id.
fn entitlement_id(account_id: &str, product_id: &str) -> Option<String> {
    crate::core::db()
        .ok()?
        .list_games(account_id)
        .inspect_err(|err| tracing::warn!(account_id, %err, "failed to read the cached amazon games"))
        .ok()?
        .into_iter()
        .find(|game| game.storefront == Storefront::AmazonGames && game.product_id == product_id)
        .and_then(|game| game.entitlement_id)
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

/// Installs, carries on or repairs the game under `key` as a tracked row. `library_root` picks the
/// library folder of a new install; an existing one stays where it is.
pub async fn download(
    key: DownloadKey,
    title: String,
    kind: DownloadKind,
    library_root: Option<std::path::PathBuf>,
    state: &AppState,
    cx: &mut AsyncApp,
) {
    let DownloadKey { account_id, product_id } = &key;
    let is_repair = kind == DownloadKind::Repair;
    tracing::info!(account_id, product_id, title, is_repair, "amazon download requested");
    let Some(entitlement_id) = entitlement_id(account_id, product_id) else {
        tracing::error!(
            account_id,
            product_id,
            "no cached entitlement id, the amazon download can't start"
        );
        fail_row(
            state,
            &key,
            &title,
            kind,
            Text::new("amazon.install.no_entitlement"),
            cx,
        );
        return;
    };
    let library = match crate::core::library_for_account(account_id) {
        Ok(library) => match library_root.as_deref() {
            Some(root) => library.targeting(root),
            None => library,
        },
        Err(err) => {
            tracing::error!(account_id, product_id, %err, "could not resolve the library, amazon download aborted");
            fail_row(state, &key, &title, kind, Text::new("library.install.no_library"), cx);
            return;
        }
    };
    let (on_progress, progress) = progress_channel();
    let target = decanter_amazon::Target::new(account_id, product_id, &entitlement_id, &title);
    let work = async {
        if is_repair {
            decanter_amazon::repair(&library, target, on_progress).await
        } else {
            decanter_amazon::install(&library, target, on_progress).await
        }
    };
    let job = DownloadJob {
        key: key.clone(),
        title: title.clone(),
        kind,
    };
    if matches!(track(state, job, progress, work, cx).await, Ended::Done(())) {
        tracing::info!(account_id, product_id, is_repair, "amazon download finished");
    }
    refresh_install_state(state, account_id, product_id, cx);
}

/// Deletes the stopped download of `key`'s game, the confirmed cancel of a paused row.
pub async fn discard(library: &Library, key: &DownloadKey) -> Result<(), Text> {
    decanter_amazon::discard(library, &key.account_id, &key.product_id)
        .await
        .inspect_err(|err| {
            tracing::error!(
                account_id = key.account_id,
                product_id = key.product_id,
                error = %format_args!("{err:#}"),
                "deleting a paused amazon download failed"
            );
        })
        .map_err(Text::verbatim)
}

/// Uninstalls the game, and its paused download with it.
pub async fn uninstall(account_id: &str, product_id: &str, state: &AppState, cx: &mut AsyncApp) -> Result<(), Text> {
    tracing::info!(account_id, product_id, "amazon uninstall started");
    let library = crate::core::library_for_account(account_id)
        .map_err(|err| Text::new("library.install.library_error").text("error", err.localized()))?;
    decanter_amazon::uninstall(&library, account_id, product_id)
        .await
        .inspect_err(|err| {
            tracing::error!(account_id, product_id, error = %format_args!("{err:#}"), "amazon uninstall failed");
        })
        .map_err(Text::verbatim)?;
    let key = DownloadKey::new(account_id, product_id);
    state.background.update(cx, |background, cx| {
        if background
            .installing
            .get(&key)
            .is_some_and(|row| matches!(row.status, InstallStatus::Paused { .. }))
        {
            background.installing.remove(&key);
            cx.notify();
        }
    });
    refresh_install_state(state, account_id, product_id, cx);
    tracing::info!(account_id, product_id, "amazon uninstall finished");
    Ok(())
}

/// Verifies the game against the manifest it was installed from, in the shape Archivos locales
/// shows for every store.
pub async fn verify(
    library: &Library,
    account_id: &str,
    product_id: &str,
) -> anyhow::Result<decanter_gog::VerifyReport> {
    let report = decanter_amazon::verify(library, account_id, product_id).await?;
    let broken = report
        .broken
        .into_iter()
        .map(|file| decanter_gog::BrokenFile {
            relative_path: file.path,
            owner: product_id.to_owned(),
            reason: match file.reason {
                decanter_amazon::BrokenReason::Missing => decanter_gog::BrokenReason::Missing,
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

/// Fetches again what is missing or wrong in the installed game, as a tracked row's work.
pub async fn repair(
    library: &Library,
    account_id: &str,
    product_id: &str,
    title: &str,
    on_progress: impl FnMut(decanter_gog::Progress) + Send + 'static,
) -> anyhow::Result<()> {
    let entitlement_id = entitlement_id(account_id, product_id)
        .ok_or_else(|| anyhow::anyhow!("no Amazon entitlement is cached for this game yet"))?;
    let target = decanter_amazon::Target::new(account_id, product_id, &entitlement_id, title);
    decanter_amazon::repair(library, target, on_progress).await
}
