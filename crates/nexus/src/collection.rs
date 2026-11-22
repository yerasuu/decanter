//! A Nexus Mods collection read as a list of mod files, to become a pack. Read through the v2
//! `GraphQL` `collectionRevision` query (checked by hand against the public endpoint: it answers
//! `modFiles` with each file's ids, version and `optional` flag, and says nothing of load order or of
//! the installer choices Vortex replays, so the list's own order is all that is kept).

use serde_json::{Value, json};

use crate::id::{GameDomain, NexusModId, display_text};
use crate::search::GRAPHQL_URL;
use crate::{Error, FileId, Result};

const CALL: &str = "the collection";
const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// A collection holds a few hundred mods at most; more is not one decanter should apply.
const MAX_FILES: usize = 1000;
const MAX_TEXT_CHARS: usize = 200;
const MAX_SLUG: usize = 32;

/// Sent as a fixed document with variables, so that nothing the user types becomes `GraphQL`.
const QUERY: &str = "query DecanterCollection($slug: String!, $domain: String, $revision: Int) { \
    collectionRevision(slug: $slug, domainName: $domain, revision: $revision, viewAdultContent: true) { \
    revisionNumber collection { name summary user { name } game { domainName } } \
    modFiles { fileId optional version file { name version sizeInBytes mod { modId name } } } } }";

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CollectionRef {
    pub domain: Option<GameDomain>,
    pub slug: String,
    /// `None` for the latest.
    pub revision: Option<u32>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Collection {
    pub name: String,
    pub summary: Option<String>,
    pub author: Option<String>,
    pub domain: GameDomain,
    pub revision: u32,
    pub files: Vec<CollectionFile>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CollectionFile {
    pub mod_id: NexusModId,
    pub file: FileId,
    pub name: String,
    pub version: Option<String>,
    pub size: Option<u64>,
    /// The collection's author marks it as not needed for the collection to work.
    pub optional: bool,
}

/// What a pasted collection address or slug names.
///
/// # Errors
///
/// [`Error::InvalidReference`] for anything that is not a Nexus Mods collection address or a bare slug.
#[inline]
pub fn parse_collection_reference(pasted: &str) -> Result<CollectionRef> {
    let text = pasted.trim();
    let invalid = || Error::InvalidReference(display_text(text, MAX_TEXT_CHARS));
    if is_slug(text) {
        return Ok(CollectionRef {
            domain: None,
            slug: text.to_owned(),
            revision: None,
        });
    }
    let url = url::Url::parse(text).map_err(|_err| invalid())?;
    let is_site = url.scheme() == "https"
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("www.nexusmods.com") || host.eq_ignore_ascii_case("nexusmods.com")
        });
    if !is_site {
        return Err(invalid());
    }
    let segments: Vec<&str> = url.path_segments().ok_or_else(invalid)?.collect();
    // /games/<domain>/collections/<slug>[/revisions/<n>]
    let [games, domain, collections, slug, rest @ ..] = segments.as_slice() else {
        return Err(invalid());
    };
    if *games != "games" || *collections != "collections" || !is_slug(slug) {
        return Err(invalid());
    }
    let revision = match rest {
        [] | ["mods" | "changelog" | "bugs" | "comments", ..] => None,
        ["revisions", number, ..] => Some(number.parse::<u32>().map_err(|_err| invalid())?),
        _ => return Err(invalid()),
    };
    Ok(CollectionRef {
        domain: Some(domain.parse().map_err(|_err| invalid())?),
        slug: (*slug).to_owned(),
        revision,
    })
}

fn is_slug(text: &str) -> bool {
    (1..=MAX_SLUG).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// The files of `reference`'s collection.
///
/// # Errors
///
/// As a request, [`Error::NotFound`] when there is no such collection or revision, and
/// [`Error::Malformed`] when the answer has no file that holds up or more than 1000.
#[inline]
pub async fn collection(client: &crate::Client, reference: &CollectionRef) -> Result<Collection> {
    let url = url::Url::parse(GRAPHQL_URL).map_err(|_err| Error::malformed(CALL, "the endpoint is not a URL"))?;
    let body = json!({
        "query": QUERY,
        "variables": {
            "slug": reference.slug,
            "domain": reference.domain.as_ref().map(GameDomain::as_str),
            "revision": reference.revision,
        },
    })
    .to_string();
    let answer = client.post_graphql(url, body, CALL, MAX_BYTES).await?;
    read_collection(&answer)
}

fn read_collection(bytes: &[u8]) -> Result<Collection> {
    let answer: Value = serde_json::from_slice(bytes).map_err(|err| Error::malformed(CALL, err.to_string()))?;
    if let Some(errors) = answer
        .get("errors")
        .and_then(Value::as_array)
        .filter(|errors| !errors.is_empty())
    {
        let code = errors
            .first()
            .and_then(|error| error.pointer("/extensions/code"))
            .and_then(Value::as_str);
        if code == Some("NOT_FOUND") {
            return Err(Error::NotFound { call: CALL });
        }
        let first = errors
            .first()
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(|message| display_text(message, MAX_TEXT_CHARS))
            .unwrap_or_default();
        return Err(Error::malformed(
            CALL,
            format!("{} errors, first: {first}", errors.len()),
        ));
    }
    let revision = answer
        .pointer("/data/collectionRevision")
        .filter(|found| found.is_object())
        .ok_or_else(|| Error::malformed(CALL, "no collection in the answer"))?;
    let text = |pointer: &str| {
        revision
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(|given| display_text(given, MAX_TEXT_CHARS))
            .filter(|shown| !shown.is_empty())
    };
    let domain = text("/collection/game/domainName")
        .and_then(|domain| domain.parse().ok())
        .ok_or_else(|| Error::malformed(CALL, "no game in the collection"))?;
    let listed = revision
        .get("modFiles")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::malformed(CALL, "no files in the collection"))?;
    if listed.len() > MAX_FILES {
        return Err(Error::malformed(CALL, "too many files"));
    }
    let mut skipped = 0_usize;
    let mut files = Vec::with_capacity(listed.len());
    for node in listed {
        match file(node) {
            Some(read) => files.push(read),
            None => skipped = skipped.saturating_add(1),
        }
    }
    if files.is_empty() {
        return Err(Error::malformed(CALL, "no file of the collection holds up"));
    }
    tracing::info!(files = files.len(), skipped, "nexus collection read");
    Ok(Collection {
        name: text("/collection/name").unwrap_or_else(|| "Collection".to_owned()),
        summary: text("/collection/summary"),
        author: text("/collection/user/name"),
        domain,
        revision: revision
            .get("revisionNumber")
            .and_then(Value::as_u64)
            .and_then(|number| u32::try_from(number).ok())
            .unwrap_or_default(),
        files,
    })
}

fn file(node: &Value) -> Option<CollectionFile> {
    let text = |pointer: &str| {
        node.pointer(pointer)
            .and_then(Value::as_str)
            .map(|given| display_text(given, MAX_TEXT_CHARS))
            .filter(|shown| !shown.is_empty())
    };
    Some(CollectionFile {
        mod_id: NexusModId::new(node.pointer("/file/mod/modId")?.as_u64()?)?,
        file: FileId::new(node.get("fileId")?.as_u64()?)?,
        name: text("/file/mod/name").or_else(|| text("/file/name"))?,
        version: text("/version").or_else(|| text("/file/version")),
        size: node.pointer("/file/sizeInBytes").and_then(|size| {
            size.as_u64()
                .or_else(|| size.as_str().and_then(|text| text.parse().ok()))
        }),
        optional: node.get("optional").and_then(Value::as_bool).unwrap_or_default(),
    })
}
