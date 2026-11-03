// SPDX-License-Identifier: GPL-3.0-or-later
//! One game's user stats with their definitions, from Galaxy's `gameplay.gog.com`, read only.
//!
//! GOG does not document this endpoint. Its path is the achievements one with `stats` in place
//! of `achievements` (comet's `api/gog/stats.rs`, read in `docs/comet-integration-analysis.md`
//! §1.4), but keyed by the game's own Galaxy client id and borne by a session scoped to that game:
//! the owner's read-only probe on 2026-11-03 (kb #1516) had GOG refuse the account session with
//! the product id in the path (HTTP 403) and answer `{"total_count": N, "items": [...]}` this way,
//! with `items` empty, not a 404, for a game that has no stats. That probe verified each item's
//! `stat_id` (a numeric string), `stat_key`, `name`, `type`, `value`, `window` (null or a number),
//! `increment_only`, `max_change`, `min_value`, `max_value` and `default_value` (null or a
//! number), as the SDK's `IStats` describes them
//! ([Galaxy SDK: IStats](https://docs.gog.com/galaxyapi/classgalaxy_1_1api_1_1IStats.html)).
//! Only `int` was seen, `float` and `avgrate` are the SDK's other two types. `name` is not read,
//! since `stat_definitions` has no column for it and the SDK addresses stats by key.

use decanter_core::{StatDefinition, StatKind, StatValue};
use serde::Deserialize;
use serde_json::Number;

use super::Error;
use super::auth::is_account_id;
use super::http::{Call, Host, SESSION_REFUSED, Sessionless};

const STATS: Call = Call {
    name: "user stats",
    refused: SESSION_REFUSED,
    max_body: 4 << 20,
    has_secrets: false,
};

/// Far above any game's stat count, so that a runaway answer is refused rather than stored.
const MAX_STATS: usize = 4096;

/// Keys and ids end up as primary keys and in what a local service answers the game.
const MAX_ID_LEN: usize = 256;

/// The client id comes from a build manifest and becomes a path segment.
const MAX_CLIENT_ID_LEN: usize = 64;

#[derive(Deserialize)]
struct Item {
    stat_id: String,
    stat_key: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    increment_only: bool,
    #[serde(default)]
    window: Option<f64>,
    value: Option<Number>,
    #[serde(default)]
    default_value: Option<Number>,
    #[serde(default)]
    min_value: Option<Number>,
    #[serde(default)]
    max_value: Option<Number>,
    #[serde(default)]
    max_change: Option<Number>,
}

#[derive(Deserialize)]
struct Listing {
    items: Vec<Item>,
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_ID_LEN && !text.chars().any(char::is_control)
}

fn is_client_id(text: &str) -> bool {
    (1..=MAX_CLIENT_ID_LEN).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// An int stat must be a whole number in the SDK's `int32` range and a float stat a finite one;
/// anything else is not a value the SDK could hand the game.
fn stat_value(kind: StatKind, number: &Number) -> Option<StatValue> {
    match kind {
        StatKind::Int => number
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .map(StatValue::Int),
        StatKind::Float | StatKind::AvgRate => number.as_f64().filter(|value| value.is_finite()).map(StatValue::Float),
    }
}

/// `None` for an item no local service could serve: an unknown type, a key or id that cannot be
/// one, or a value of the wrong kind. A bound of the wrong kind is dropped, not the item.
fn checked(item: Item) -> Option<(StatDefinition, StatValue)> {
    let kind = StatKind::from_name(&item.kind)?;
    if !is_identifier(&item.stat_key) || !is_identifier(&item.stat_id) {
        return None;
    }
    let value = stat_value(kind, item.value.as_ref()?)?;
    let bound = |bound: Option<Number>| stat_value(kind, &bound?);
    let mut definition = StatDefinition::new(item.stat_key, item.stat_id, kind);
    definition.increment_only = item.increment_only;
    definition.window = item.window.filter(|window| window.is_finite() && *window >= 0.0_f64);
    definition.default_value = bound(item.default_value);
    definition.min_value = bound(item.min_value);
    definition.max_value = bound(item.max_value);
    definition.max_change = bound(item.max_change);
    Some((definition, value))
}

impl Sessionless {
    /// Every stat of the game whose Galaxy client is `client_id`, with the value `user_id` has
    /// for it. `game_token` must be a session exchanged for that same client: GOG refuses the
    /// account's own. GOG answers the whole list at once, so no paging is done.
    ///
    /// Items no local service could serve are left out and counted in a warning.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a client id or account id that cannot be a path segment,
    /// before anything is sent; [`Error::Rejected`] when GOG refuses `game_token`;
    /// [`Error::Malformed`] for more than 4096 items or an answer of another shape; otherwise
    /// [`Error::Network`] or [`Error::Unavailable`].
    #[inline]
    pub async fn user_stats(
        &self,
        client_id: &str,
        user_id: &str,
        game_token: &str,
    ) -> Result<Vec<(StatDefinition, StatValue)>, Error> {
        if !is_client_id(client_id) {
            return Err(Error::InvalidInput { what: "game client id" });
        }
        if !is_account_id(user_id) {
            return Err(Error::InvalidInput { what: "account id" });
        }
        let path = format!("/clients/{client_id}/users/{user_id}/stats");
        let listing: Listing = self.get(&STATS, Host::Gameplay, &path, &[], Some(game_token)).await?;
        if listing.items.len() > MAX_STATS {
            return Err(Error::Malformed {
                call: STATS.name,
                reason: format!("more than {MAX_STATS} stats"),
            });
        }
        let listed = listing.items.len();
        let stats: Vec<_> = listing.items.into_iter().filter_map(checked).collect();
        let skipped = listed.saturating_sub(stats.len());
        if skipped > 0 {
            tracing::warn!(
                client_id,
                listed,
                skipped,
                "GOG listed user stats that cannot be served"
            );
        }
        Ok(stats)
    }
}
