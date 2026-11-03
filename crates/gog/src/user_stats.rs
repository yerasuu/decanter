//! Seeds a game's user stats into the local tables before a launch, so that a local Galaxy
//! service can answer `GetUserStats` when GOG cannot be reached (comet's `preload`, see
//! `docs/comet-internals-gap-analysis.md` G15).
//!
//! GOG answers a game's stats only to a session scoped to that game, keyed by the build's own
//! Galaxy client id, the exchange cloud saves already makes: the owner's read-only probe on
//! 2026-11-03 (kb #1516) had the account's own session refused with HTTP 403 on the product id
//! path, and this one answered. See `api::client::stats` for the shape it verified.

use anyhow::Context as _;
use decanter_core::{Db, StatDefinition, StatValue, Storefront};
use reqwest::StatusCode;

use crate::api::client::{Error, Sessionless, Token};
use crate::{cloud_saves, session};

/// Refreshes one game's stats from GOG, or succeeds leaving them as stored when GOG is unreachable.
///
/// Unreachable is offline, a timeout, a 5xx, a 404 or a 429, at the session check, the build
/// lookup, the game session exchange or the stats read. Values recorded locally and not synced
/// yet are never overwritten (see [`Db::refresh_user_stats`]). A game with no stats is a
/// successful fetch of none. Returns whether GOG answered.
///
/// # Errors
///
/// A product id that is not numeric, no stored session, GOG refusing the session or the game
/// session exchange, a build with no Galaxy client, GOG answering something unreadable, and the
/// database refusing the write.
#[inline]
pub async fn seed_user_stats(db: &mut Db, account_id: &str, product_id: &str) -> anyhow::Result<bool> {
    let numeric_id: i64 = product_id.parse().context("GOG product_id must be numeric")?;

    let checked = crate::achv_try!(
        session::with_client(account_id, async |client| {
            match client.user().await {
                Ok(_) => Ok(Some(client.token().await)),
                Err(err @ (Error::Network { .. } | Error::Unavailable { .. })) => {
                    tracing::info!(account_id, product_id, %err, "GOG unreachable, user stats left as stored");
                    Ok(None)
                }
                Err(err) => Err(session::failed("GOG session is not valid", err)),
            }
        })
        .await
    );
    let Some(live) = checked else {
        return Ok(false);
    };

    let stats = match fetch(&live, numeric_id).await {
        Ok(stats) => stats,
        Err(err) if is_unreachable(&err) => {
            tracing::info!(
                account_id,
                product_id,
                err = format!("{err:#}"),
                "GOG unreachable, user stats left as stored"
            );
            return Ok(false);
        }
        // Flattened, since callers show only the outermost message.
        Err(err) => return Err(anyhow::anyhow!("GOG user stats request failed: {err:#}")),
    };

    let kept_local = db.refresh_user_stats(account_id, Storefront::Gog, product_id, &stats)?;
    tracing::info!(
        account_id,
        product_id,
        stat_count = stats.len(),
        kept_local,
        "seeded GOG user stats"
    );
    Ok(true)
}

/// Keeps every error typed in its chain, so that [`is_unreachable`] can read it.
async fn fetch(live: &Token, game_id: i64) -> anyhow::Result<Vec<(StatDefinition, StatValue)>> {
    let game = cloud_saves::exchange_game_token(live, game_id).await?;
    // Otherwise another account's stats would be stored under this one.
    anyhow::ensure!(
        game.user_id == live.user_id,
        "GOG exchanged a game session for another account"
    );
    let http = Sessionless::new()?;
    Ok(http
        .user_stats(&game.client_id, &game.user_id, game.access_token.expose())
        .await?)
}

/// Reads unreachable as the rest of the crate does wherever it happened: the GOG client's own
/// errors say so, and the build lookup and session exchange carry reqwest's.
fn is_unreachable(failure: &anyhow::Error) -> bool {
    failure.chain().any(|cause| {
        cause.downcast_ref::<Error>().map_or_else(
            || {
                cause.downcast_ref::<reqwest::Error>().is_some_and(|http| {
                    http.status().map_or_else(
                        || http.is_connect() || http.is_timeout() || http.is_request() || http.is_body(),
                        |status| {
                            status.is_server_error()
                                || status == StatusCode::NOT_FOUND
                                || status == StatusCode::TOO_MANY_REQUESTS
                        },
                    )
                })
            },
            |gog| matches!(gog, Error::Network { .. } | Error::Unavailable { .. }),
        )
    })
}
