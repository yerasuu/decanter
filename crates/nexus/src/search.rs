// SPDX-License-Identifier: GPL-3.0-or-later
//! Searching a game's mods by name through Nexus' `GraphQL` v2 `mods` query, kept apart and off.
//!
//! The [v2 reference](https://graphql.nexusmods.com/) lists `mods(filter, sort, offset, count)`
//! with `ModsFilter.name`, `gameDomainName` and `adultContent` and the `ModsSort` fields used here,
//! but not whether `mods` needs a token, what it costs against the hourly allowance, or the exact
//! shape of a filter value and a node (design, sections 2.2 and 13). Until one hand-made request
//! and Nexus' own answer settle that, [`SEARCH_V2`] stays `false`: nothing here sends anything, the
//! app never builds a request, and the search stays the v1 lists filtered in the app plus opening a
//! mod by number. What is here is the request body and the reading of an answer, so that turning it
//! on is a transport and a switch, not a design.

use serde_json::{Value, json};

use crate::id::{GameDomain, NexusModId, checked_picture, date, display_text};
use crate::listing::ModInfo;
use crate::{Error, Result};

/// Off until the spike and Nexus' answer (design, section 2.2); see the module's documentation.
pub const SEARCH_V2: bool = false;
/// The v2 endpoint as the reference names it.
pub const GRAPHQL_URL: &str = "https://api.nexusmods.com/v2/graphql";
/// Results a page asks for, the design's budget of one request for twenty.
pub const SEARCH_PAGE: u32 = 20;

const CALL: &str = "the mod search";
const MAX_QUERY_CHARS: usize = 100;
const MAX_NODES: usize = 100;
const MAX_NAME_CHARS: usize = 200;
const MAX_SUMMARY_CHARS: usize = 600;
const MAX_FIELD_CHARS: usize = 100;

/// Sent as a fixed document with variables, so that nothing the user types becomes `GraphQL`.
const QUERY: &str = "query DecanterModSearch($filter: ModsFilter, $sort: [ModsSort!], $offset: Int, $count: Int) { \
    mods(filter: $filter, sort: $sort, offset: $offset, count: $count) { \
    totalCount nodes { modId name summary author version endorsements updatedAt pictureUrl adultContent } } }";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SearchSort {
    #[default]
    Relevance,
    Downloads,
    Endorsements,
    Updated,
    Created,
    Name,
}

impl SearchSort {
    const fn field(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Downloads => "downloads",
            Self::Endorsements => "endorsements",
            Self::Updated => "updatedAt",
            Self::Created => "createdAt",
            Self::Name => "name",
        }
    }

    const fn direction(self) -> &'static str {
        match self {
            Self::Name => "ASC",
            Self::Relevance | Self::Downloads | Self::Endorsements | Self::Updated | Self::Created => "DESC",
        }
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SearchPage {
    pub mods: Vec<ModInfo>,
    pub total: Option<u64>,
    pub offset: u32,
}

/// The JSON body of a v2 search of `domain`'s mods whose name matches `text`, from `offset`.
#[must_use]
#[inline]
pub fn search_body(domain: &GameDomain, text: &str, sort: SearchSort, hide_adult: bool, offset: u32) -> Value {
    let wanted: String = text.trim().chars().take(MAX_QUERY_CHARS).collect();
    let mut filter = serde_json::Map::new();
    filter.insert(
        "gameDomainName".to_owned(),
        json!([{ "value": domain.as_str(), "op": "EQUALS" }]),
    );
    if !wanted.is_empty() {
        filter.insert("name".to_owned(), json!([{ "value": wanted, "op": "WILDCARD" }]));
    }
    if hide_adult {
        filter.insert("adultContent".to_owned(), json!([{ "value": false, "op": "EQUALS" }]));
    }
    let mut order = serde_json::Map::new();
    order.insert(sort.field().to_owned(), json!({ "direction": sort.direction() }));
    json!({
        "query": QUERY,
        "variables": {
            "filter": filter,
            "sort": [order],
            "offset": offset,
            "count": SEARCH_PAGE,
        },
    })
}

/// The mods of a v2 search answer; a node that does not hold up is skipped and counted.
///
/// # Errors
///
/// [`Error::Malformed`] when the body is not JSON, carries `GraphQL` `errors`, or has no `mods`.
#[inline]
pub fn read_search(body: &[u8], offset: u32) -> Result<SearchPage> {
    let answer: Value = serde_json::from_slice(body)
        .map_err(|err| Error::malformed(CALL, format!("line {}, column {}", err.line(), err.column())))?;
    if let Some(errors) = answer
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        let first = errors
            .first()
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(|message| display_text(message, MAX_FIELD_CHARS))
            .unwrap_or_default();
        return Err(Error::malformed(
            CALL,
            format!("{} errors, first: {first}", errors.len()),
        ));
    }
    let found = answer
        .get("data")
        .and_then(|data| data.get("mods"))
        .ok_or_else(|| Error::malformed(CALL, "no mods in the answer"))?;
    let nodes = found
        .get("nodes")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut skipped = 0_usize;
    let mut mods = Vec::with_capacity(nodes.len().min(MAX_NODES));
    for node in nodes.iter().take(MAX_NODES) {
        match node_info(node) {
            Some(info) => mods.push(info),
            None => skipped = skipped.saturating_add(1),
        }
    }
    tracing::info!(offset, found = mods.len(), skipped, "nexus v2 search answer read");
    Ok(SearchPage {
        mods,
        total: found.get("totalCount").and_then(Value::as_u64),
        offset,
    })
}

fn node_info(node: &Value) -> Option<ModInfo> {
    let text = |key: &str, max: usize| {
        node.get(key)
            .and_then(Value::as_str)
            .map(|given| display_text(given, max))
            .filter(|shown| !shown.is_empty())
    };
    Some(ModInfo {
        id: NexusModId::new(node.get("modId")?.as_u64()?)?,
        name: text("name", MAX_NAME_CHARS)?,
        summary: text("summary", MAX_SUMMARY_CHARS).unwrap_or_default(),
        picture: node.get("pictureUrl").and_then(Value::as_str).and_then(checked_picture),
        version: text("version", MAX_FIELD_CHARS),
        author: text("author", MAX_FIELD_CHARS),
        author_page: None,
        endorsements: node.get("endorsements").and_then(Value::as_u64).unwrap_or_default(),
        updated: node.get("updatedAt").and_then(Value::as_str).and_then(date),
        has_adult_content: node.get("adultContent").and_then(Value::as_bool).unwrap_or_default(),
        is_available: true,
    })
}
