//! A self-contained GOG client: auth, library listing, installs, cloud
//! saves, and achievement polling. Replaces bottles-next's `Library`/
//! `Install` gRPC surface — nothing here talks to `bottles-server`.
//!
//! Shared between `achv-sidecar` (CLI, explicit or post-launch polling)
//! and the UI (auto-poll when a game is selected in Biblioteca, so nobody
//! has to run a command by hand to see progress). Keyed by profile, not by
//! prefix: unlock progress lives on the GOG account, independent of which
//! prefix launches it.

use achv_core::{CachedAchievement, Db, Storefront};
use anyhow::Context;

mod auth;
pub mod cloud_saves;
mod depot;
mod gamesdb;
mod install;
mod library;
mod orders;
mod product;
pub mod save_location;
mod updates;

pub use auth::{Account, auth_url, extract_code, log_in, log_out, session};
pub use depot::Build;
pub use install::{
    BrokenFile, BrokenReason, Progress, VerifyReport, cancel, install, repair, uninstall, uninstall_dlc, verify,
};
pub use library::{Batch, GameKind, LibraryEntry, enrich_games, find_game, list_games};
pub use orders::purchase_dates;
pub use updates::{Branch, check_all_updates, check_update, list_branches, list_builds};

/// Propagates a `Result`'s error out of the enclosing `anyhow::Result` fn,
/// same as `?` — kept as a macro so every call site across this crate's mix
/// of error types (reqwest, `gog`'s error-chain-based errors mapped by
/// hand, serde_json, rusqlite via achv-core) reads the same way.
#[macro_export]
macro_rules! achv_try {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(err) => return Err(::core::convert::Into::into(err)),
        }
    };
}

/// This profile's stored GOG session, as the raw JSON `write_session` saved
/// it — `achv_core::credentials` keyed by [`Storefront::Gog`], wrapped in
/// `spawn_blocking` since the OS keyring is a blocking D-Bus round trip.
async fn read_session(profile_id: &str) -> anyhow::Result<Option<String>> {
    let profile_id = profile_id.to_owned();
    achv_try!(
        tokio::task::spawn_blocking(move || achv_core::load_session(&profile_id, Storefront::Gog))
            .await
            .context("session-read worker task panicked")
    )
    .context("reading GOG session from keyring")
}

/// Stores this profile's GOG session as serialized JSON.
async fn write_session(profile_id: &str, session_json: String) -> anyhow::Result<()> {
    let profile_id = profile_id.to_owned();
    achv_try!(
        tokio::task::spawn_blocking(move || achv_core::save_session(&profile_id, Storefront::Gog, &session_json))
            .await
            .context("session-write worker task panicked")
    )
    .context("writing GOG session to keyring")
}

/// Drops this profile's stored GOG session.
async fn clear_session(profile_id: &str) -> anyhow::Result<()> {
    let profile_id = profile_id.to_owned();
    achv_try!(
        tokio::task::spawn_blocking(move || achv_core::delete_session(&profile_id, Storefront::Gog))
            .await
            .context("session-clear worker task panicked")
    )
    .context("clearing GOG session from keyring")
}

/// Runs `f` against a live, authenticated `gog::Gog` client built from the
/// stored session, then persists whatever `f` leaves in `client.token` —
/// any authenticated call may have refreshed it, and a caller that skipped
/// this would silently log itself out one token rotation later.
///
/// `f` runs inside `spawn_blocking`: `gog::Gog` holds its token in a
/// `RefCell`, so it isn't `Send` across an `.await` the way an async
/// closure would need.
async fn with_session<T, F>(profile_id: &str, f: F) -> anyhow::Result<T>
where
    F: FnOnce(&gog::Gog) -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let stored = read_session(profile_id)
        .await?
        .context("no GOG session for this profile — log in first")?;
    let profile_id = profile_id.to_owned();
    let (result, token) = achv_try!(achv_try!(
        tokio::task::spawn_blocking(move || -> anyhow::Result<(T, gog::token::Token)> {
            let token: gog::token::Token =
                achv_try!(serde_json::from_str(&stored).context("stored GOG session is corrupt"));
            let client = gog::Gog::new(token);
            let result = achv_try!(f(&client));
            Ok((result, client.token.borrow().clone()))
        })
        .await
        .context("GOG worker task panicked")
    ));

    let refreshed = achv_try!(serde_json::to_string(&token).context("serializing the refreshed GOG session"));
    write_session(&profile_id, refreshed).await?;

    Ok(result)
}

/// Polls GOG, updates the local cache, and returns whichever achievements
/// just transitioned to unlocked (empty if none — that's the common case,
/// not an error).
pub async fn poll_achievements(
    db: &mut Db,
    profile_id: &str,
    product_id: &str,
) -> anyhow::Result<Vec<CachedAchievement>> {
    let numeric_id: i64 = product_id.parse().context("GOG product_id must be numeric")?;

    let list = with_session(profile_id, move |client| {
        let uid = client.uid();
        // gog::Error is built on error-chain and isn't Sync, so it can't
        // cross the `?`-to-anyhow boundary directly.
        client
            .achievements(numeric_id, uid)
            .map_err(|err| anyhow::anyhow!("gog achievements request failed: {err}"))
    })
    .await?;

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

    Ok(db.upsert_achievements(profile_id, Storefront::Gog, product_id, &items)?)
}
