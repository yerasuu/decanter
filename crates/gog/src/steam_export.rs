//! Downloads artwork and hands it, along with a launch spec, to
//! `decanter_core::steam` — the only networked half of exporting an
//! installed game as a real Steam shortcut. See that module for the
//! actual `shortcuts.vdf`/grid-artwork writing; this file only fetches
//! bytes and calls it.
//!
//! Best-effort throughout: a missing Steam install, an unresolvable local
//! user, or a failed image download each cost artwork or the export
//! itself, never the install/uninstall that triggered this — none of
//! this ever propagates as a reason for that to fail.

use std::path::{Path, PathBuf};

use anyhow::Context;
use decanter_core::{Db, InstalledGame, Storefront};

/// Downloads whatever artwork URLs are `Some`, then writes or updates this
/// game's Steam shortcut and persists the assigned appid.
///
/// # Errors
/// Fails if the appid can't be persisted to `db` after a successful
/// write — the shortcut itself would then exist with no recorded way to
/// find it again for removal.
pub async fn sync_steam_shortcut(
    db: &Db,
    account_id: &str,
    game: &InstalledGame,
    install_dir: &Path,
    cover_url: Option<&str>,
    background_url: Option<&str>,
    icon_url: Option<&str>,
) -> anyhow::Result<()> {
    let Some(steam_root) = decanter_core::steam::steam_root() else {
        tracing::debug!(product_id = %game.game_id, "no Steam installation found, skipping Steam export");
        return Ok(());
    };
    let Some(user_id) = decanter_core::steam::local_user_id(&steam_root) else {
        tracing::warn!(
            product_id = %game.game_id,
            "Steam is installed but no local user could be resolved, skipping Steam export"
        );
        return Ok(());
    };
    let Some(executable) = game.launch_executable.as_deref() else {
        tracing::debug!(product_id = %game.game_id, "no launch executable recorded, skipping Steam export");
        return Ok(());
    };

    let temp_dir = std::env::temp_dir().join(format!("decanter-steam-artwork-{account_id}-{}", game.game_id));
    if let Err(err) = tokio::fs::create_dir_all(&temp_dir).await {
        tracing::warn!(%err, "could not create a temp dir for artwork downloads, skipping Steam export");
        return Ok(());
    }

    let client = crate::http_client();
    let icon = download_artwork(&client, icon_url, &temp_dir, "icon").await;
    // `cover_url` is GOG's portrait crop (see `decanter_gog::library::enrich`)
    // — there is no separate landscape capsule source, so only the
    // portrait grid slot is filled; the legacy landscape slot is simply
    // left blank rather than stretching a portrait image into it.
    let portrait = download_artwork(&client, cover_url, &temp_dir, "portrait").await;
    let hero = download_artwork(&client, background_url, &temp_dir, "hero").await;

    let exe = install_dir.join(executable);
    let start_dir = install_dir.to_path_buf();
    let name = game.name.clone();
    let artwork = decanter_core::steam::Artwork {
        icon,
        capsule: None,
        portrait,
        hero,
    };

    let appid = tokio::task::spawn_blocking(move || {
        let spec = decanter_core::steam::ShortcutSpec {
            name: &name,
            exe: &exe,
            start_dir: &start_dir,
        };
        decanter_core::steam::sync(&steam_root, &user_id, &spec, &artwork)
    })
    .await
    .context("steam shortcut sync worker task panicked")?
    .context("writing steam shortcut")?;

    if let Err(err) = tokio::fs::remove_dir_all(&temp_dir).await {
        tracing::debug!(path = %temp_dir.display(), %err, "could not clean up artwork temp dir");
    }

    db.set_steam_shortcut_appid(account_id, Storefront::Gog, &game.game_id, appid)
        .context("persisting steam shortcut appid")?;
    tracing::info!(account_id, product_id = %game.game_id, appid, "synced steam shortcut");
    Ok(())
}

/// Removes this game's Steam shortcut and grid artwork, if one was ever
/// recorded — a no-op otherwise (nothing to remove, or Steam/the appid
/// mapping no longer resolve, which is harmless: the row is simply left
/// behind rather than failing an uninstall over it).
///
/// # Errors
/// Fails if the recorded appid can't be read from `db`, or the shortcut
/// removal itself fails.
pub async fn remove_steam_shortcut(db: &Db, account_id: &str, product_id: &str) -> anyhow::Result<()> {
    let Some(appid) = db
        .steam_shortcut_appid(account_id, Storefront::Gog, product_id)
        .context("reading steam shortcut appid")?
    else {
        tracing::debug!(account_id, product_id, "no steam shortcut recorded, nothing to remove");
        return Ok(());
    };
    let Some(steam_root) = decanter_core::steam::steam_root() else {
        tracing::debug!(
            account_id,
            product_id,
            "no Steam installation found, leaving the appid record in place"
        );
        return Ok(());
    };
    let Some(user_id) = decanter_core::steam::local_user_id(&steam_root) else {
        tracing::debug!(
            account_id,
            product_id,
            "no local Steam user resolved, leaving the appid record in place"
        );
        return Ok(());
    };

    tokio::task::spawn_blocking(move || decanter_core::steam::remove(&steam_root, &user_id, appid))
        .await
        .context("steam shortcut removal worker task panicked")?
        .context("removing steam shortcut")?;

    db.remove_steam_shortcut_appid(account_id, Storefront::Gog, product_id)
        .context("clearing steam shortcut appid record")?;
    tracing::info!(account_id, product_id, appid, "removed steam shortcut");
    Ok(())
}

/// Downloads `url` (if `Some`) into `{temp_dir}/{filename_stem}.<ext>`,
/// `<ext>` sniffed from the URL itself. `None` in (no URL) or any failure
/// along the way both come back as `None` — an artwork slot decanter
/// simply doesn't fill this time, not a reason to abort the sync.
async fn download_artwork(
    client: &reqwest::Client,
    url: Option<&str>,
    temp_dir: &Path,
    filename_stem: &str,
) -> Option<PathBuf> {
    let url = url?;
    let response = match client.get(url).send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!(url, %err, "artwork download failed");
            return None;
        }
    };
    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!(url, %err, "artwork request returned an error status");
            return None;
        }
    };
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::debug!(url, %err, "failed to read artwork response body");
            return None;
        }
    };

    let path = temp_dir.join(format!("{filename_stem}.{}", extension_from_url(url)));
    match tokio::fs::write(&path, &bytes).await {
        Ok(()) => Some(path),
        Err(err) => {
            tracing::debug!(path = %path.display(), %err, "failed to write downloaded artwork to a temp file");
            None
        }
    }
}

/// GOG serves mostly `.jpg`, occasionally `.png` (see
/// `crate::product::image_url`'s own extension check) — sniffed from the
/// URL rather than the response, since GOG's CDN doesn't reliably set
/// `Content-Type` for these.
fn extension_from_url(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    if path.ends_with(".png") {
        "png"
    } else if path.ends_with(".jpeg") {
        "jpeg"
    } else {
        "jpg"
    }
}
