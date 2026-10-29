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
use decanter_core::{CachedAchievement, Db, Storefront};
use tokio::sync::Mutex as AsyncMutex;

// Upstream style, kept as published (see api/VENDOR.md): the workspace's clippy groups are not applied to it.
#[allow(clippy::all, clippy::pedantic, clippy::restriction, clippy::nursery)]
#[rustfmt::skip]
pub mod api;
mod auth;
pub mod cloud_saves;
mod depot;
pub mod hook;
mod install;
mod library;
mod mods;
mod orders;
mod product;
pub mod save_location;
mod session;
mod steam_export;
mod updates;

pub use auth::{Account, auth_url, extract_code, log_in, log_out};
pub use depot::Build;
pub use install::{
    BrokenFile, BrokenReason, Progress, VerifyReport, cancel, install, repair, uninstall, uninstall_dlc, verify,
};
pub use library::{Batch, GameKind, LibraryEntry, enrich_games, find_game, list_games};
pub use mods::{GogMod, gog_mods};
pub use orders::purchase_dates;
pub use session::{SessionState, check_session, renew_session};
pub use steam_export::{remove_steam_shortcut, sync_steam_shortcut};
pub use updates::{Branch, check_all_updates, check_update, list_branches, list_builds};

/// Propagates a `Result`'s error out of the enclosing `anyhow::Result` fn,
/// same as `?` — kept as a macro so every call site across this crate's mix
/// of error types (reqwest, `api`'s error-chain-based errors mapped by
/// hand, serde_json, rusqlite via decanter-core) reads the same way.
#[macro_export]
macro_rules! achv_try {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(err) => return Err(::core::convert::Into::into(err)),
        }
    };
}

/// `api`'s own requests carry the refresh token or the login code in the URL, and reqwest puts
/// the URL in its errors; every `api` error goes through this before it is worded or logged.
pub(crate) fn without_secrets(err: api::Error) -> api::Error {
    match err {
        api::Error(api::ErrorKind::Network(network), state) => {
            api::Error(api::ErrorKind::Network(network.without_url()), state)
        }
        other => other,
    }
}

/// The client every one-shot API call in this crate goes through.
///
/// reqwest's own default has no timeout at all, so a response GOG's embed
/// API or GamesDB stalls on — no error, just silence — hangs the caller
/// forever instead of failing. `install::download` hit the same failure
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

/// This account's stored GOG session, as the raw JSON `write_session` saved
/// it — `decanter_core::credentials` keyed by [`Storefront::Gog`], wrapped in
/// `spawn_blocking` since the OS keyring is a blocking D-Bus round trip.
async fn read_session(account_id: &str) -> anyhow::Result<Option<String>> {
    let account_id = account_id.to_owned();
    let for_worker = account_id.clone();
    let session = achv_try!(
        tokio::task::spawn_blocking(move || decanter_core::load_session(&for_worker, Storefront::Gog))
            .await
            .context("session-read worker task panicked")
    )
    .context("reading GOG session from keyring")?;
    tracing::debug!(%account_id, found = session.is_some(), "read GOG session from keyring");
    Ok(session)
}

/// Stores this account's GOG session as serialized JSON.
async fn write_session(account_id: &str, session_json: String) -> anyhow::Result<()> {
    let session_bytes = session_json.len();
    let account_id = account_id.to_owned();
    let for_worker = account_id.clone();
    achv_try!(
        tokio::task::spawn_blocking(move || decanter_core::save_session(&for_worker, Storefront::Gog, &session_json))
            .await
            .context("session-write worker task panicked")
    )
    .context("writing GOG session to keyring")?;
    tracing::debug!(%account_id, session_bytes, "wrote GOG session to keyring");
    Ok(())
}

/// Drops this account's stored GOG session.
async fn clear_session(account_id: &str) -> anyhow::Result<()> {
    let account_id = account_id.to_owned();
    let for_worker = account_id.clone();
    achv_try!(
        tokio::task::spawn_blocking(move || decanter_core::delete_session(&for_worker, Storefront::Gog))
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
/// [`with_session`] calls for the *same* account refreshes first leaves the
/// other holding an already-spent refresh token, which GOG then reports as
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

/// Runs `f` against a live, authenticated `api::Gog` client built from the
/// stored session, then persists whatever `f` leaves in `client.token` —
/// any authenticated call may have refreshed it, and a caller that skipped
/// this would silently log itself out one token rotation later. That holds
/// when `f` fails too: the `api` client refreshes before the request that then
/// fails, and GOG may have spent the old refresh token by then.
///
/// `f` runs inside `spawn_blocking`: `api::Gog` holds its token in a
/// `RefCell`, so it isn't `Send` across an `.await` the way an async
/// closure would need.
async fn with_session<T, F>(account_id: &str, f: F) -> anyhow::Result<T>
where
    F: FnOnce(&api::Gog) -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let lock = session_lock(account_id);
    let _guard = lock.lock().await;

    let stored = read_session(account_id).await?;
    if stored.is_none() {
        tracing::debug!(account_id, "no GOG session stored for this account");
    }
    let stored = stored.context("no GOG session for this account — log in first")?;
    let started: api::token::Token = achv_try!(serde_json::from_str(&stored).context("stored GOG session is corrupt"));
    let (result, token) = achv_try!(
        tokio::task::spawn_blocking(move || {
            let client = api::Gog::new(started);
            let result = f(&client);
            (result, client.token.into_inner())
        })
        .await
        .context("GOG worker task panicked")
    );

    let ended = achv_try!(serde_json::to_string(&token).context("serializing the refreshed GOG session"));
    if ended != stored {
        write_session(account_id, ended).await?;
        tracing::debug!(
            account_id,
            succeeded = result.is_ok(),
            "persisted refreshed GOG session"
        );
    }

    result
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

    let list = with_session(account_id, move |client| {
        let uid = client.uid();
        // api::Error is built on error-chain and isn't Sync, so it can't
        // cross the `?`-to-anyhow boundary directly.
        client
            .achievements(numeric_id, uid)
            .map_err(|err| anyhow::anyhow!("gog achievements request failed: {}", crate::without_secrets(err)))
    })
    .await?;
    tracing::debug!(
        account_id,
        product_id,
        achievement_count = list.items.len(),
        "fetched GOG achievements"
    );

    let items: Vec<CachedAchievement> = list
        .items
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
