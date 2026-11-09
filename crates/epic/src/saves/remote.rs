// SPDX-License-Identifier: GPL-3.0-or-later
//! The datastorage calls of spec 9: the listing of a title's save files, the links an upload is
//! put through, and the anonymous transfers over the pre-signed links both hand out.
//!
//! Every key and link in Epic's answers is untrusted. A key becomes nothing on disk: it is checked
//! against the shape this module reads and only ever compared with keys decanter builds. A link
//! must be HTTPS to a named host and is never logged, only its host is, since its query is a
//! signature (spec, 13.14).

use core::time::Duration;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDateTime;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value};
use url::Url;

use super::SaveError;
use crate::error::CallError;
use crate::hosts::Host;
use crate::http::{self, Call, Kind};
use crate::install::distribution::{self, Fetched};
use crate::session::{self, Live, Session};

const LISTING: Call = Call {
    name: "cloud saves listing",
    host: Host::CloudSaves,
    kind: Kind::Bearer,
    max_body: 16 << 20,
};

const WRITE_LINKS: Call = Call {
    name: "cloud saves upload links",
    host: Host::CloudSaves,
    kind: Kind::Bearer,
    max_body: 4 << 20,
};

pub(super) const MAX_SNAPSHOT_MANIFEST: usize = 16 << 20;
const MAX_ENTRIES: usize = 200_000;
const MAX_KEY_LEN: usize = 512;
/// The segments of the shortest key spec 9 describes, a manifest's, and a ceiling past it for the
/// chunk paths of 6.4, which add up to three.
const MANIFEST_SEGMENTS: usize = 5;
const MAX_SEGMENTS: usize = 8;
/// The index of the `appName` segment and of the folder after it (spec, 9).
const APP_SEGMENT: usize = 2;
const KIND_SEGMENT: usize = 3;
const SNAPSHOT_TIME: &str = "%Y.%m.%d-%H.%M.%S";
const TRANSFER_TIMEOUT: Duration = Duration::from_mins(2);
const MAX_LOGGED_HOSTS: usize = 8;

/// A snapshot's manifest as the listing names it.
pub(super) struct SnapshotEntry {
    /// The file name, `<YYYY.MM.DD-HH.MM.SS>.manifest`.
    pub(super) name: String,
    /// The key's first three segments, which the snapshot's chunks are read under.
    pub(super) prefix: String,
    /// Unix seconds, from the name in UTC.
    pub(super) time: i64,
    pub(super) link: Option<Url>,
}

/// A title's save files, checked.
pub(super) struct Listing {
    /// Oldest first.
    pub(super) snapshots: Vec<SnapshotEntry>,
    /// Every chunk key, with its read link when it has one.
    chunks: HashMap<String, Option<Url>>,
}

impl Listing {
    pub(super) fn newest(&self) -> Option<&SnapshotEntry> {
        self.snapshots.last()
    }

    pub(super) fn has_snapshot(&self, name: &str) -> bool {
        self.snapshots.iter().any(|snapshot| snapshot.name == name)
    }

    pub(super) fn has_chunk(&self, prefix: &str, path: &str) -> bool {
        self.chunks.contains_key(&format!("{prefix}/{path}"))
    }

    pub(super) fn chunk_link(&self, prefix: &str, path: &str) -> Option<&Url> {
        self.chunks.get(&format!("{prefix}/{path}"))?.as_ref()
    }

    pub(super) fn chunk_count(&self) -> usize {
        self.chunks.len()
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    files: Option<Map<String, Value>>,
}

/// What did not fit the reading of spec 9, counted for one log line at the end of a listing.
#[derive(Default)]
struct Misfits {
    bad_key: usize,
    other_app: usize,
    unknown_kind: usize,
    bad_time: usize,
    no_link: usize,
    bad_link: usize,
    hosts: BTreeSet<String>,
}

/// The title's listing (spec, 9), and the session it was read with for an upload to go on with.
pub(super) async fn list(account_id: &str, app_name: &str) -> Result<(Listing, Session), SaveError> {
    let path = format!("/api/v1/access/egstore/savesync/{account_id}/{app_name}/");
    let (answer, session) = authorized(account_id, |client, session| {
        let request = http::request(client, Method::GET, &LISTING, &path).bearer_auth(session.access_token().expose());
        http::send::<Answer>(request, &LISTING)
    })
    .await?;
    let listing = read_listing(app_name, answer)?;
    Ok((listing, session))
}

fn read_listing(app_name: &str, answer: Answer) -> Result<Listing, SaveError> {
    let Some(files) = answer.files else {
        tracing::warn!(app_name, "epic cloud saves listing has no files member, refused");
        return Err(SaveError::UnexpectedShape("the listing has no files"));
    };
    if files.len() > MAX_ENTRIES {
        return Err(SaveError::TooLarge {
            what: "listing entries",
            limit: u64::try_from(MAX_ENTRIES).unwrap_or(u64::MAX),
        });
    }
    let entries = files.len();
    let mut misfits = Misfits::default();
    let mut snapshots = Vec::new();
    let mut chunks = HashMap::with_capacity(entries);
    for (key, entry) in files {
        let Some(segments) = segments(&key) else {
            misfits.bad_key = misfits.bad_key.saturating_add(1);
            continue;
        };
        if segments.get(APP_SEGMENT).copied() != Some(app_name) {
            misfits.other_app = misfits.other_app.saturating_add(1);
            continue;
        }
        let link = match read_link(entry.get("readLink")) {
            Ok(Some(link)) => {
                if misfits.hosts.len() < MAX_LOGGED_HOSTS {
                    misfits.hosts.extend(link.host_str().map(str::to_owned));
                }
                Some(link)
            }
            Ok(None) => {
                misfits.no_link = misfits.no_link.saturating_add(1);
                None
            }
            Err(()) => {
                misfits.bad_link = misfits.bad_link.saturating_add(1);
                None
            }
        };
        let kind = segments.get(KIND_SEGMENT).copied().unwrap_or_default();
        if kind == "manifests" {
            let found = match segments.as_slice() {
                [_, _, _, _, name] => snapshot_time(name).map(|time| (*name, time)),
                _ => None,
            };
            let Some((name, time)) = found else {
                misfits.bad_time = misfits.bad_time.saturating_add(1);
                continue;
            };
            snapshots.push(SnapshotEntry {
                name: name.to_owned(),
                prefix: segments.get(..KIND_SEGMENT).unwrap_or_default().join("/"),
                time,
                link,
            });
        } else if kind.starts_with("Chunks") && key.ends_with(".chunk") {
            chunks.insert(key, link);
        } else {
            misfits.unknown_kind = misfits.unknown_kind.saturating_add(1);
        }
    }
    snapshots.sort_by(|left, right| left.time.cmp(&right.time).then_with(|| left.name.cmp(&right.name)));
    // Entries under another folder of the title are only counted; a key that does not split as
    // spec 9 says, names another title or a manifest decanter cannot date, or a link it will not
    // fetch, means the listing is not read as Epic meant it.
    let misread = misfits
        .bad_key
        .saturating_add(misfits.other_app)
        .saturating_add(misfits.bad_time)
        .saturating_add(misfits.bad_link);
    let prefixes: BTreeSet<&str> = snapshots.iter().map(|snapshot| snapshot.prefix.as_str()).collect();
    tracing::info!(
        app_name,
        entries,
        snapshots = snapshots.len(),
        chunks = chunks.len(),
        prefixes = prefixes.len(),
        bad_key = misfits.bad_key,
        other_app = misfits.other_app,
        unknown_kind = misfits.unknown_kind,
        bad_time = misfits.bad_time,
        no_link = misfits.no_link,
        bad_link = misfits.bad_link,
        link_hosts = ?misfits.hosts,
        "listed epic cloud saves"
    );
    // Acting on part of a listing could take an older snapshot for the newest one, so any misread
    // entry, or entries of which none is a snapshot or a chunk, refuse it whole.
    if misread > 0 {
        return Err(SaveError::UnexpectedShape(
            "some entries of the listing do not have the expected path",
        ));
    }
    if entries > 0 && snapshots.is_empty() && chunks.is_empty() {
        return Err(SaveError::UnexpectedShape(
            "no entry of the listing has the expected path",
        ));
    }
    if snapshots.last().is_some_and(|newest| newest.link.is_none()) {
        return Err(SaveError::UnexpectedShape("the newest snapshot has no read link"));
    }
    if prefixes.len() > 1 {
        return Err(SaveError::UnexpectedShape(
            "the snapshots sit under more than one prefix",
        ));
    }
    Ok(Listing { snapshots, chunks })
}

/// The key's segments when every one is a plain name of the characters spec 9's keys use, `None`
/// otherwise.
fn segments(key: &str) -> Option<Vec<&str>> {
    if key.is_empty() || key.len() > MAX_KEY_LEN {
        return None;
    }
    let segments: Vec<&str> = key.split('/').collect();
    let is_plain = |segment: &&str| {
        !matches!(*segment, "" | "." | "..")
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b':'))
    };
    ((MANIFEST_SEGMENTS..=MAX_SEGMENTS).contains(&segments.len()) && segments.iter().all(is_plain)).then_some(segments)
}

fn snapshot_time(name: &str) -> Option<i64> {
    let stem = name.strip_suffix(".manifest")?;
    NaiveDateTime::parse_from_str(stem, SNAPSHOT_TIME)
        .ok()
        .map(|time| time.and_utc().timestamp())
}

/// `Ok(None)` for no link, `Err` for one decanter will not fetch: not HTTPS, no host name, or
/// credentials in it.
fn read_link(value: Option<&Value>) -> Result<Option<Url>, ()> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value.as_str().ok_or(())?;
    let url = Url::parse(text).map_err(|_| ())?;
    let is_fit =
        url.scheme() == "https" && url.domain().is_some() && url.username().is_empty() && url.password().is_none();
    if is_fit { Ok(Some(url)) } else { Err(()) }
}

/// Links to put `paths` through, each a path relative to the title's folder as spec 9 names them
/// (`manifests/<name>` and the chunk paths of 6.4). The answer's keys are taken as given back, or
/// as the full key ending in the path, since which of the two Epic answers is UNVERIFIED.
pub(super) async fn write_links(
    account_id: &str,
    app_name: &str,
    session: Session,
    paths: &[&str],
) -> Result<HashMap<String, Url>, SaveError> {
    let path = format!("/api/v1/access/egstore/savesync/{account_id}/{app_name}/");
    let body = serde_json::to_vec(&serde_json::json!({ "files": paths }))
        .map_err(|err| SaveError::Local(format!("writing the upload request: {err}")))?;
    let answer = authorized_with(account_id, session, |client, session| {
        let request = http::request(client, Method::POST, &WRITE_LINKS, &path)
            .bearer_auth(session.access_token().expose())
            .header(CONTENT_TYPE, "application/json")
            .body(body.clone());
        http::send::<Answer>(request, &WRITE_LINKS)
    })
    .await?
    .0;
    let Some(files) = answer.files else {
        tracing::warn!(
            app_name,
            asked = paths.len(),
            "epic upload links answer has no files member, nothing put"
        );
        return Err(SaveError::UnexpectedShape("the upload links answer has no files"));
    };
    let mut by_path: BTreeMap<&str, Url> = BTreeMap::new();
    let (mut unmatched, mut bad_link) = (0_usize, 0_usize);
    for (key, entry) in &files {
        let wanted = paths
            .iter()
            .find(|wanted| key == **wanted || key.strip_suffix(**wanted).is_some_and(|head| head.ends_with('/')));
        let Some(wanted) = wanted else {
            unmatched = unmatched.saturating_add(1);
            continue;
        };
        match read_link(entry.get("writeLink")) {
            Ok(Some(link)) => {
                by_path.insert(wanted, link);
            }
            Ok(None) | Err(()) => bad_link = bad_link.saturating_add(1),
        }
    }
    let missing = paths.len().saturating_sub(by_path.len());
    tracing::info!(
        app_name,
        asked = paths.len(),
        answered = files.len(),
        linked = by_path.len(),
        missing,
        unmatched,
        bad_link,
        "epic cloud saves upload links read"
    );
    if missing > 0 {
        return Err(SaveError::UnexpectedShape(
            "the upload links answer lacks a path asked for",
        ));
    }
    Ok(by_path
        .into_iter()
        .map(|(path, link)| (path.to_owned(), link))
        .collect())
}

/// `call` with the account's session, renewed and tried once more when Epic refuses its token
/// (spec, 13.4).
async fn authorized<T, F, Fut>(account_id: &str, call: F) -> Result<(T, Session), SaveError>
where
    F: Fn(&reqwest::Client, &Session) -> Fut,
    Fut: Future<Output = Result<T, CallError>>,
{
    let session = match session::live(account_id).await.map_err(|err| local(&err))? {
        Live::Ready(session) => session,
        Live::Closed => return Err(SaveError::SessionClosed),
        Live::Unreachable(reason) => return Err(SaveError::Unreachable(reason)),
    };
    authorized_with(account_id, session, call).await
}

async fn authorized_with<T, F, Fut>(account_id: &str, mut session: Session, call: F) -> Result<(T, Session), SaveError>
where
    F: Fn(&reqwest::Client, &Session) -> Fut,
    Fut: Future<Output = Result<T, CallError>>,
{
    let client = http::client().map_err(|err| SaveError::Local(session::reason(err)))?;
    let mut answer = call(&client, &session).await;
    if matches!(answer, Err(CallError::Revoked { .. })) {
        session = match session::renew_refused(&session).await.map_err(|err| local(&err))? {
            Live::Ready(fresh) => fresh,
            Live::Closed => return Err(SaveError::SessionClosed),
            Live::Unreachable(reason) => return Err(SaveError::Unreachable(reason)),
        };
        answer = call(&client, &session).await;
    }
    match answer {
        Ok(answer) => Ok((answer, session)),
        Err(err) => {
            tracing::warn!(account_id, error = %err, "epic cloud saves call failed");
            Err(call_error(err))
        }
    }
}

fn call_error(err: CallError) -> SaveError {
    match err {
        CallError::Revoked { .. } | CallError::ActionRequired { .. } => SaveError::Refused(session::reason(err)),
        CallError::Malformed { .. } => SaveError::UnexpectedShape("an answer is not the expected JSON"),
        CallError::TimedOut { .. }
        | CallError::Unreachable { .. }
        | CallError::Moved { .. }
        | CallError::Unavailable { .. } => SaveError::Unreachable(session::reason(err)),
        CallError::Setup(_) => SaveError::Local(session::reason(err)),
    }
}

fn local(err: &anyhow::Error) -> SaveError {
    SaveError::Local(format!("{err:#}"))
}

/// The client of the pre-signed links: no `Authorization` (spec, 2.3), the CDN's own limits.
pub(super) fn anonymous() -> Result<reqwest::Client, SaveError> {
    distribution::cdn_client().map_err(|err| SaveError::Local(session::reason(err)))
}

/// The body behind a read link, of at most `limit` bytes.
pub(super) async fn fetch(client: &reqwest::Client, link: Option<&Url>, limit: usize) -> Result<Vec<u8>, SaveError> {
    let link = link.ok_or(SaveError::UnexpectedShape("a file has no read link"))?;
    let host = link.host_str().unwrap_or_default();
    distribution::fetch_capped(client, link, limit, TRANSFER_TIMEOUT)
        .await
        .map_err(|err| {
            tracing::warn!(host, error = %err, "epic cloud save file not fetched");
            match err {
                Fetched::TooLarge => SaveError::TooLarge {
                    what: "cloud save file",
                    limit: u64::try_from(limit).unwrap_or(u64::MAX),
                },
                // The links are pre-signed and their lifetime is unknown (spec, Q22): a refusal may be
                // one that expired, which a new listing replaces.
                Fetched::Refused(status) => SaveError::Unreachable(format!("a read link answered HTTP {status}")),
                Fetched::RateLimited(_) => SaveError::Unreachable("a read link answered HTTP 429".to_owned()),
                Fetched::Network(reason) => SaveError::Unreachable(reason),
            }
        })
}

/// Puts `body` through a write link. Only 200, 201 and 204 count as stored.
pub(super) async fn put(client: &reqwest::Client, link: &Url, body: Vec<u8>) -> Result<(), SaveError> {
    let host = link.host_str().unwrap_or_default();
    let response = client
        .put(link.clone())
        .timeout(TRANSFER_TIMEOUT)
        .body(body)
        .send()
        .await
        .map_err(|err| {
            let reason = err.without_url().to_string();
            tracing::warn!(host, reason, "epic cloud save upload failed");
            SaveError::Unreachable(reason)
        })?;
    let status = response.status();
    if matches!(status, StatusCode::OK | StatusCode::CREATED | StatusCode::NO_CONTENT) {
        return Ok(());
    }
    tracing::warn!(host, status = status.as_u16(), "epic cloud save upload refused");
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        Err(SaveError::Unreachable(format!(
            "a write link answered HTTP {}",
            status.as_u16()
        )))
    } else {
        Err(SaveError::Refused(format!(
            "a write link answered HTTP {}",
            status.as_u16()
        )))
    }
}
