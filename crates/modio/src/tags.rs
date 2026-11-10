// SPDX-License-Identifier: GPL-3.0-or-later
//! A game's tag options (`GET /games/{id}/tags`), which a search can filter by.
//!
//! The endpoint is in the REST reference, but the design could not read its answer's shape
//! (section 13): each entry is read for a `name`, a `tags` list of strings and a `hidden` flag and
//! nothing more, an entry without a name or tags is skipped and counted, and hidden groups are left
//! out, since a game hides them from its own users.

use serde::Deserialize;

use crate::Result;
use crate::games::RawPage;
use crate::http::Client;
use crate::id::{GameId, display_text};

const CALL: &str = "the game's tags";
const MAX_PAGE_BYTES: u64 = 2 << 20;
const MAX_GROUPS: usize = 50;
const MAX_TAGS: usize = 200;
const MAX_NAME_CHARS: usize = 100;

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct TagGroup {
    pub name: String,
    pub tags: Vec<String>,
}

#[derive(Deserialize)]
struct RawGroup {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tags: Vec<serde_json::Value>,
    #[serde(default)]
    hidden: bool,
}

/// The tag groups `game` shows its users, in mod.io's order.
///
/// # Errors
///
/// As a request, or [`Error::Malformed`](crate::Error::Malformed) when the answer is not a page.
#[inline]
pub async fn tag_options(client: &Client, game: GameId) -> Result<Vec<TagGroup>> {
    let page: RawPage = client
        .api(&format!("/games/{game}/tags"), &[], CALL, MAX_PAGE_BYTES)
        .await?;
    let mut skipped = 0_usize;
    let mut groups = Vec::with_capacity(page.data.len().min(MAX_GROUPS));
    for value in page.data.into_iter().take(MAX_GROUPS) {
        let Ok(raw) = serde_json::from_value::<RawGroup>(value) else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        if raw.hidden {
            continue;
        }
        let name = raw
            .name
            .as_deref()
            .map(|text| display_text(text, MAX_NAME_CHARS))
            .unwrap_or_default();
        let tags: Vec<String> = raw
            .tags
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|text| display_text(text, MAX_NAME_CHARS))
            .filter(|text| !text.is_empty())
            .take(MAX_TAGS)
            .collect();
        if name.is_empty() || tags.is_empty() {
            skipped = skipped.saturating_add(1);
            continue;
        }
        groups.push(TagGroup { name, tags });
    }
    tracing::info!(game_id = %game, groups = groups.len(), skipped, "modio tag options read");
    Ok(groups)
}
