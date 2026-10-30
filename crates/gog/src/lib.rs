//! A self-contained GOG client: auth, library listing, installs, cloud
//! saves, and achievement polling — all in-process, with no external
//! service to reach the same public endpoints through.
//!
//! Shared between `decanter-sidecar` (CLI, explicit or post-launch polling)
//! and the UI (auto-poll when a game is selected in Biblioteca, so nobody
//! has to run a command by hand to see progress). Keyed by account, not by
//! prefix: unlock progress lives on the GOG account, independent of which
//! prefix launches it. This crate has no notion of a profile grouping
//! several accounts — see `decanter_core::profiles` for that.

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::Context;
use decanter_core::secrets::{self, Secret, SecretKey};
use decanter_core::{CachedAchievement, Db, Storefront};
use tokio::sync::Mutex as AsyncMutex;

pub mod api;
mod auth;
pub mod cloud_saves;
mod depot;
pub mod hook;
mod install;
mod library;
mod mods;
mod orders;
pub mod save_location;
mod session;
mod steam_export;
mod updates;

pub use api::client::Build;
pub use auth::{Account, auth_url, extract_code, log_in, log_out};
pub use install::{
    BrokenFile, BrokenReason, BuildSwitch, Cancelled, NothingToResume, PauseReason, Paused, Progress, Room,
    StrayStaging, UnfinishedDownload, VerifyReport, cancel, discard, discard_stray, downloading_dir, install, pause,
    pause_for_quit, repair, resume, resume_repair, stray_staging, unfinished_downloads, uninstall, uninstall_dlc,
    verify,
};
pub use library::{Batch, GameKind, LibraryEntry, enrich_games, find_game, list_games};
pub use mods::{GogMod, gog_mods};
pub use orders::purchase_dates;
pub use session::{SessionState, check_session, renew_session};
pub use steam_export::{remove_steam_shortcut, sync_steam_shortcut};
pub use updates::{Branch, check_all_updates, check_update, list_branches, list_builds};

/// Propagates a `Result`'s error out of the enclosing `anyhow::Result` fn,
/// same as `?` — kept as a macro so every call site across this crate's mix
/// of error types (reqwest, `api::client::Error`, serde_json, rusqlite via
/// decanter-core) reads the same way.
#[macro_export]
macro_rules! achv_try {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(err) => return Err(::core::convert::Into::into(err)),
        }
    };
}

/// The client for the one-shot requests this crate sends outside
/// `api::client`: cloud storage and its per-game token, remote config and
/// the Steam export's artwork.
///
/// reqwest's own default has no timeout at all, so a response one of those
/// hosts stalls on — no error, just silence — hangs the caller forever
/// instead of failing. `install::download` hit the same failure
/// mode against CDN chunk transfers and picked idle-read timeouts instead,
/// since a chunk can legitimately take a while; every call here is a small
/// JSON round trip, so one flat timeout on the whole request is enough.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|err| {
            // Was a silent fallback to a client with no timeout at all — the one
            // failure mode this function exists to avoid — so it's worth a warn
            // even though `Client::builder().build()` failing is rare in practice.
            tracing::warn!(%err, "failed to build GOG HTTP client with a timeout, falling back to reqwest's default");
            reqwest::Client::default()
        })
}

/// This account's stored GOG session, as the raw JSON `write_session` saved it, from
/// `decanter_core::secrets` in `spawn_blocking`, since the OS keyring is a blocking D-Bus round
/// trip.
async fn read_session(account_id: &str) -> anyhow::Result<Option<Secret>> {
    let key = SecretKey::gog_session(account_id)?;
    let session = achv_try!(
        tokio::task::spawn_blocking(move || secrets::store().get(&key))
            .await
            .context("session-read worker task panicked")
    )
    .context("reading GOG session from keyring")?;
    tracing::debug!(%account_id, found = session.is_some(), "read GOG session from keyring");
    Ok(session)
}

/// Stores this account's GOG session as serialized JSON.
async fn write_session(account_id: &str, session_json: Secret) -> anyhow::Result<()> {
    let session_bytes = session_json.expose().len();
    let key = SecretKey::gog_session(account_id)?;
    achv_try!(
        tokio::task::spawn_blocking(move || secrets::store().put(&key, &session_json))
            .await
            .context("session-write worker task panicked")
    )
    .context("writing GOG session to keyring")?;
    tracing::debug!(%account_id, session_bytes, "wrote GOG session to keyring");
    Ok(())
}

/// Drops this account's stored GOG session.
async fn clear_session(account_id: &str) -> anyhow::Result<()> {
    let key = SecretKey::gog_session(account_id)?;
    achv_try!(
        tokio::task::spawn_blocking(move || secrets::store().delete(&key))
            .await
            .context("session-clear worker task panicked")
    )
    .context("clearing GOG session from keyring")?;
    tracing::debug!(%account_id, "cleared GOG session from keyring");
    Ok(())
}

/// One lock per account, handed out from a registry guarded by a plain
/// `Mutex` (held only long enough to look up or insert the entry, never
/// across an `.await`).
///
/// GOG rotates the refresh token on use: whichever of two concurrent
/// `session::with_client` calls for the *same* account
/// refreshes first leaves the other holding an already-spent refresh token,
/// which GOG then reports as
/// revoked — not a real logout, just two callers racing to renew the same
/// session (e.g. the library's own fetch and the settings screen's session
/// check, both starting right after a fresh login). Serializing access per
/// account closes that window; unrelated accounts still run concurrently.
fn session_lock(account_id: &str) -> Arc<AsyncMutex<()>> {
    static LOCKS: OnceLock<StdMutex<HashMap<String, Arc<AsyncMutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| StdMutex::new(HashMap::new()));
    let mut locks = locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(
        locks
            .entry(account_id.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
    )
}

/// Polls GOG, updates the local cache, and returns whichever achievements
/// just transitioned to unlocked (empty if none — that's the common case,
/// not an error).
pub async fn poll_achievements(
    db: &mut Db,
    account_id: &str,
    product_id: &str,
) -> anyhow::Result<Vec<CachedAchievement>> {
    let numeric_id: i64 = product_id.parse().context("GOG product_id must be numeric")?;

    let list = achv_try!(
        session::with_client(account_id, async |client| {
            client
                .achievements(numeric_id)
                .await
                .map_err(|err| session::failed("GOG achievements request failed", err))
        })
        .await
    );
    tracing::debug!(
        account_id,
        product_id,
        achievement_count = list.len(),
        "fetched GOG achievements"
    );

    let items: Vec<CachedAchievement> = list
        .into_iter()
        .map(|a| CachedAchievement {
            achievement_key: a.achievement_key,
            name: a.name,
            description: Some(a.description),
            icon_unlocked_url: Some(a.image_url_unlocked),
            icon_locked_url: Some(a.image_url_locked),
            unlocked: a.date_unlocked.is_some(),
            unlocked_at: a.date_unlocked,
        })
        .collect();

    let unlocked = db.upsert_achievements(account_id, Storefront::Gog, product_id, &items)?;
    if !unlocked.is_empty() {
        tracing::info!(
            account_id,
            product_id,
            newly_unlocked = unlocked.len(),
            "GOG achievements unlocked"
        );
    }
    Ok(unlocked)
}
