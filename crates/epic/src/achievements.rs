// SPDX-License-Identifier: GPL-3.0-or-later
//! One title's achievements and the account's unlocks, read from the store's `/graphql` endpoint
//! (spec, section 10) into decanter's achievement cache. Read only: games unlock through the EOS
//! SDK, never through the launcher, so nothing here detects or reports an unlock.
//!
//! The two query documents are decanter's own, selecting only the members the spec names. How
//! the operations take their variables (as field arguments of the same name, `String!`) is
//! UNVERIFIED, as is the whole field set (spec, Q21): an answer that does not have the expected
//! shape is a [`CallError::Malformed`] that leaves the cache as it was, logged with counts, so the
//! owner's first real title settles it.

use std::collections::HashMap;

use anyhow::Context as _;
use chrono::DateTime;
use decanter_core::secrets::Secret;
use decanter_core::{CachedAchievement, Db, Storefront};
use reqwest::Method;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Map, Value, json};

use crate::error::CallError;
use crate::hosts::Host;
use crate::http::{self, Call, Kind};
use crate::library::catalog::https_image;
use crate::library::{CatalogRegion, is_id, printable};
use crate::session::{self, Live, Session};

const DEFINITIONS: Call = Call {
    name: "achievement definitions",
    host: Host::Store,
    kind: Kind::Bearer,
    max_body: 8 << 20,
};
const PLAYER: Call = Call {
    name: "player achievements",
    host: Host::Store,
    kind: Kind::Bearer,
    max_body: 4 << 20,
};
const GRAPHQL_PATH: &str = "/graphql";

const DEFINITIONS_QUERY: &str = "query Achievement($sandboxId: String!, $locale: String!) { Achievement { \
     productAchievementsRecordBySandbox(sandboxId: $sandboxId, locale: $locale) { sandboxId totalAchievements \
     achievements { achievement { name hidden unlockedDisplayName lockedDisplayName unlockedDescription \
     lockedDescription unlockedIconLink lockedIconLink rarity { percent } } } } } }";
// The launcher's own request also sends a `locale` this operation does not declare (spec, 10);
// whether the server tolerates that is open, so it is left out.
const PLAYER_QUERY: &str = "query PlayerAchievement($epicAccountId: String!, $sandboxId: String!) { \
     PlayerAchievement { playerAchievementGameRecordsBySandbox(epicAccountId: $epicAccountId, \
     sandboxId: $sandboxId) { records { totalUnlocked playerAchievements { playerAchievement { \
     achievementName unlocked unlockDate } } } } } }";

const MAX_ACHIEVEMENTS: usize = 2_000;
/// The key is matched between the two answers and stored as the cache's key, so it is kept
/// verbatim or not at all.
const KEY_MAX_LEN: usize = 128;
const NAME_MAX_CHARS: usize = 256;
const DESCRIPTION_MAX_CHARS: usize = 2048;
const DATE_MAX_LEN: usize = 64;

/// What one poll counted, logged once at its end: the answers' shape is UNVERIFIED (spec, Q21).
#[derive(Default)]
struct Tally {
    defined: usize,
    /// Definitions that are not objects or have no usable `name`.
    malformed: usize,
    repeated: usize,
    over_cap: usize,
    /// Icon links that were not bounded HTTPS addresses.
    icons_dropped: usize,
    unlocks: usize,
    /// Player records that are not objects or have no usable `achievementName`.
    unlocks_malformed: usize,
    /// Unlocks naming an achievement the definitions do not list.
    unlocks_unmatched: usize,
    /// `unlockDate` texts that are neither RFC 3339 nor `N/A`.
    dates_unreadable: usize,
}

/// Writes `app_name`'s achievements, asked by its Epic sandbox `namespace`, to the cache.
///
/// Returns the achievements that just went from locked to unlocked. A title Epic lists no
/// achievements for leaves the cache empty and is not an error.
///
/// # Errors
///
/// A closed session, Epic not answering, an answer of an unexpected shape (a
/// [`CallError::Malformed`] in the chain, which a later poll may clear), or the cache refusing the
/// write. Nothing is written unless both answers were read.
#[inline]
pub async fn poll_achievements(
    db: &mut Db,
    account_id: &str,
    app_name: &str,
    namespace: &str,
    region: &CatalogRegion,
) -> anyhow::Result<Vec<CachedAchievement>> {
    // Both become GraphQL variables, and the namespace comes from the cached catalog.
    anyhow::ensure!(
        is_id(account_id) && is_id(namespace),
        "the cached Epic account or sandbox id is not one decanter can send"
    );
    let mut session = ready(session::live(account_id).await?)?;
    let client = http::client()?;
    let mut fetched = fetch(&client, session.access_token(), account_id, namespace, region).await;
    if matches!(fetched, Err(CallError::Revoked { .. })) {
        session = ready(session::renew_refused(&session).await?)?;
        fetched = fetch(&client, session.access_token(), account_id, namespace, region).await;
    }
    let (items, tally) =
        fetched.map_err(|err| anyhow::Error::new(err).context("Epic did not give the game's achievements"))?;
    tracing::info!(
        account_id,
        app_name,
        defined = tally.defined,
        kept = items.len(),
        malformed = tally.malformed,
        repeated = tally.repeated,
        over_cap = tally.over_cap,
        icons_dropped = tally.icons_dropped,
        unlocks = tally.unlocks,
        unlocks_malformed = tally.unlocks_malformed,
        unlocks_unmatched = tally.unlocks_unmatched,
        dates_unreadable = tally.dates_unreadable,
        "read epic achievements"
    );
    db.upsert_achievements(account_id, Storefront::EpicGames, app_name, &items)
        .context("writing the Epic achievements to the cache")
}

fn ready(live: Live) -> anyhow::Result<Session> {
    match live {
        Live::Ready(session) => Ok(session),
        Live::Closed => anyhow::bail!("Epic closed the session: log in to Epic Games again in Settings"),
        Live::Unreachable(reason) => anyhow::bail!("Epic could not be asked: {reason}"),
    }
}

async fn fetch(
    client: &reqwest::Client,
    token: &Secret,
    account_id: &str,
    namespace: &str,
    region: &CatalogRegion,
) -> Result<(Vec<CachedAchievement>, Tally), CallError> {
    let listed = graphql(
        client,
        token,
        &DEFINITIONS,
        DEFINITIONS_QUERY,
        json!({ "sandboxId": namespace, "locale": region.language() }),
    )
    .await?;
    let mut tally = Tally::default();
    let Some(mut definitions) = read_definitions(&listed, &mut tally)? else {
        return Ok((Vec::new(), tally));
    };
    // A title without achievements has no player record to ask for.
    if !definitions.rows.is_empty() {
        let player = graphql(
            client,
            token,
            &PLAYER,
            PLAYER_QUERY,
            json!({ "epicAccountId": account_id, "sandboxId": namespace }),
        )
        .await?;
        apply_unlocks(&player, &mut definitions, &mut tally)?;
    }
    let items = definitions.rows.into_iter().map(|row| row.cached).collect();
    Ok((items, tally))
}

/// The `data` member of a query's answer. A failed query comes back with a 200 and an `errors`
/// list, which here is a malformed answer: only Epic's codes are kept for the log, never its
/// messages, which may quote the variables.
async fn graphql(
    client: &reqwest::Client,
    token: &Secret,
    call: &Call,
    query: &str,
    variables: Value,
) -> Result<Map<String, Value>, CallError> {
    let body = json!({ "query": query, "variables": variables }).to_string();
    let request = http::request(client, Method::POST, call, GRAPHQL_PATH)
        .bearer_auth(token.expose())
        .header(CONTENT_TYPE, "application/json")
        .body(body);
    let mut answer: Map<String, Value> = http::send(request, call).await?;
    let raw_errors = answer.remove("errors");
    let errors = raw_errors
        .as_ref()
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if let Some(Value::Object(data)) = answer.remove("data") {
        if !errors.is_empty() {
            tracing::warn!(
                call = call.name,
                errors = errors.len(),
                codes = ?error_codes(errors),
                "epic graphql answered with data and errors, reading the data"
            );
        }
        return Ok(data);
    }
    Err(CallError::Malformed {
        call: call.name,
        reason: format!(
            "no GraphQL data, {} errors with codes {:?}",
            errors.len(),
            error_codes(errors)
        ),
    })
}

fn error_codes(errors: &[Value]) -> Vec<String> {
    errors
        .iter()
        .filter_map(|error| {
            let extensions = error.get("extensions")?;
            let code = extensions
                .get("code")
                .or_else(|| extensions.get("errorCode"))
                .and_then(Value::as_str);
            http::shown(code)
        })
        .take(4)
        .collect()
}

/// One definition while the poll runs: the cache row with Epic's locked texts, and the unlocked
/// ones it switches to once the account has it, since Epic shows a hidden achievement unhidden
/// only once unlocked (spec, 10).
struct Row {
    cached: CachedAchievement,
    unlocked_name: Option<String>,
    unlocked_description: Option<String>,
}

impl Row {
    fn unlock(&mut self, unlocked_at: Option<String>) {
        self.cached.unlocked = true;
        self.cached.unlocked_at = unlocked_at;
        if let Some(name) = self.unlocked_name.take() {
            self.cached.name = name;
        }
        if let Some(description) = self.unlocked_description.take() {
            self.cached.description = Some(description);
        }
    }
}

/// The definitions, all locked, with their index by key; `None` when Epic has no achievement
/// record for the sandbox, which a title without achievements is taken to answer with.
fn read_definitions(data: &Map<String, Value>, tally: &mut Tally) -> Result<Option<Definitions>, CallError> {
    let malformed = |reason: &str| CallError::Malformed {
        call: DEFINITIONS.name,
        reason: reason.to_owned(),
    };
    let record = data
        .get("Achievement")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("no Achievement object"))?
        .get("productAchievementsRecordBySandbox")
        .ok_or_else(|| malformed("no productAchievementsRecordBySandbox member"))?;
    if record.is_null() {
        return Ok(None);
    }
    let achievements = record
        .get("achievements")
        .ok_or_else(|| malformed("the record has no achievements member"))?;
    if achievements.is_null() {
        return Ok(None);
    }
    let entries = achievements
        .as_array()
        .ok_or_else(|| malformed("achievements is not a list"))?;
    tally.defined = entries.len();
    let capacity = entries.len().min(MAX_ACHIEVEMENTS);
    let mut definitions = Definitions {
        rows: Vec::with_capacity(capacity),
        by_key: HashMap::with_capacity(capacity),
    };
    for entry in entries {
        let Some(definition) = entry.get("achievement").and_then(Value::as_object) else {
            tally.malformed = tally.malformed.saturating_add(1);
            continue;
        };
        let Some(key) = key(definition.get("name")) else {
            tally.malformed = tally.malformed.saturating_add(1);
            continue;
        };
        if definitions.by_key.contains_key(&key) {
            tally.repeated = tally.repeated.saturating_add(1);
            continue;
        }
        if definitions.rows.len() >= MAX_ACHIEVEMENTS {
            tally.over_cap = tally.over_cap.saturating_add(1);
            continue;
        }
        definitions.by_key.insert(key.clone(), definitions.rows.len());
        definitions.rows.push(row(key, definition, tally));
    }
    // Every entry unreadable is a shape the reading got wrong, not a title without achievements.
    if definitions.rows.is_empty() && tally.malformed > 0 {
        return Err(malformed("no achievement entry has the expected members"));
    }
    Ok(Some(definitions))
}

struct Definitions {
    rows: Vec<Row>,
    by_key: HashMap<String, usize>,
}

fn row(key: String, definition: &Map<String, Value>, tally: &mut Tally) -> Row {
    let text = |member: &str, max_chars: usize| printable(definition.get(member)?.as_str()?, max_chars);
    let mut icon = |member: &str| {
        let link = definition.get(member).and_then(Value::as_str);
        let checked = https_image(link);
        if link.is_some() && checked.is_none() {
            tally.icons_dropped = tally.icons_dropped.saturating_add(1);
        }
        checked
    };
    let icon_unlocked_url = icon("unlockedIconLink");
    let icon_locked_url = icon("lockedIconLink");
    let unlocked_name = text("unlockedDisplayName", NAME_MAX_CHARS);
    let unlocked_description = text("unlockedDescription", DESCRIPTION_MAX_CHARS);
    let name = text("lockedDisplayName", NAME_MAX_CHARS)
        .or_else(|| unlocked_name.clone())
        .unwrap_or_else(|| key.clone());
    let description = text("lockedDescription", DESCRIPTION_MAX_CHARS).or_else(|| unlocked_description.clone());
    let rarity = definition
        .get("rarity")
        .and_then(|rarity| rarity.get("percent"))
        .and_then(Value::as_f64)
        .filter(|percent| (0.0_f64..=100.0_f64).contains(percent));
    Row {
        cached: CachedAchievement {
            achievement_key: key,
            name,
            description,
            icon_unlocked_url,
            icon_locked_url,
            unlocked: false,
            unlocked_at: None,
            achievement_id: None,
            visible_while_locked: definition.get("hidden").and_then(Value::as_bool).map(|hidden| !hidden),
            rarity,
            rarity_level_description: None,
            rarity_level_slug: None,
            mode: None,
        },
        unlocked_name,
        unlocked_description,
    }
}

/// Marks the rows the account unlocked. An empty `records` list is an account that has unlocked
/// nothing in the title.
fn apply_unlocks(data: &Map<String, Value>, definitions: &mut Definitions, tally: &mut Tally) -> Result<(), CallError> {
    let malformed = |reason: &str| CallError::Malformed {
        call: PLAYER.name,
        reason: reason.to_owned(),
    };
    let records = data
        .get("PlayerAchievement")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("no PlayerAchievement object"))?
        .get("playerAchievementGameRecordsBySandbox")
        .and_then(|game| game.get("records"))
        .ok_or_else(|| malformed("no playerAchievementGameRecordsBySandbox.records member"))?;
    let Some(record) = records.as_array().and_then(|list| list.first()) else {
        return Ok(());
    };
    let Some(unlocks) = record.get("playerAchievements").and_then(Value::as_array) else {
        return Err(malformed("the record has no playerAchievements list"));
    };
    for entry in unlocks.iter().take(MAX_ACHIEVEMENTS) {
        let Some(state) = entry.get("playerAchievement").and_then(Value::as_object) else {
            tally.unlocks_malformed = tally.unlocks_malformed.saturating_add(1);
            continue;
        };
        let Some(key) = key(state.get("achievementName")) else {
            tally.unlocks_malformed = tally.unlocks_malformed.saturating_add(1);
            continue;
        };
        let unlocked_at = match state.get("unlockDate").and_then(Value::as_str) {
            Some(raw) if raw.len() <= DATE_MAX_LEN && DateTime::parse_from_rfc3339(raw).is_ok() => Some(raw.to_owned()),
            Some("N/A") | None => None,
            Some(_) => {
                tally.dates_unreadable = tally.dates_unreadable.saturating_add(1);
                None
            }
        };
        let is_unlocked = state
            .get("unlocked")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| unlocked_at.is_some());
        if !is_unlocked {
            continue;
        }
        let Some(row) = definitions
            .by_key
            .get(&key)
            .and_then(|index| definitions.rows.get_mut(*index))
        else {
            tally.unlocks_unmatched = tally.unlocks_unmatched.saturating_add(1);
            continue;
        };
        tally.unlocks = tally.unlocks.saturating_add(1);
        row.unlock(unlocked_at);
    }
    Ok(())
}

fn key(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|key| {
            (1..=KEY_MAX_LEN).contains(&key.len()) && !key.chars().any(|c| c.is_control() || c.is_whitespace())
        })
        .map(str::to_owned)
}
