//! Sending a report to the report server, once and only on the player's confirmation (decisions
//! #1041, #1043, #1046).
//!
//! The server's answer is untrusted: it is read capped, checked field by field and used only when
//! every field has the shape the server documents. Neither the report nor the delete code is ever
//! logged.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use decanter_core::Db;
use reqwest::header::RETRY_AFTER;
use reqwest::{Response, StatusCode, Url, redirect};
use serde::Deserialize;

use super::sent::{self, Entry};
use super::{Built, Product};
use crate::i18n::Text;

/// Decision #1046.
pub const DEFAULT_SERVER: &str = "https://reports.yerasuu.com";

/// The advanced setting that replaces [`DEFAULT_SERVER`] in decanter's settings; an empty value turns
/// sending off. There is deliberately no environment variable for it.
pub const SERVER_SETTING: &str = "report_server";

/// The server keeps one private log per report and refuses more than this.
pub const MAX_ATTACHMENT: usize = 5 * 1024 * 1024;

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ANSWER: usize = 16 * 1024;
const MAX_REJECTIONS_SHOWN: usize = 20;
const MAX_REJECTION_PART: usize = 64;

/// Where reports go: `https://` anywhere, or plain `http://` to this machine with a port, for
/// developing against a local server. Nothing else is accepted, so no setting can send a report in
/// the clear to another machine.
#[derive(Clone)]
pub struct Server(Url);

impl Server {
    pub fn parse(address: &str) -> Result<Self, Error> {
        let invalid = || Error::Address(address.chars().take(80).collect());
        let url = Url::parse(address.trim()).map_err(|_| invalid())?;
        let local = url
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost"));
        let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && local && url.port().is_some());
        let bare = url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none();
        if scheme_ok && bare && url.host_str().is_some() {
            Ok(Self(url))
        } else {
            Err(invalid())
        }
    }

    pub fn origin(&self) -> String {
        self.0.origin().ascii_serialization()
    }

    fn endpoint(&self, path: &str) -> Result<Url, Error> {
        self.0.join(path).map_err(|_| Error::Answer)
    }
}

/// The server a report's public address belongs to, when that address is `<origin>/r/<id>` and
/// nothing else.
pub fn server_of(url: &str, id: &str) -> Option<Server> {
    let url = Url::parse(url).ok()?;
    if url.path() != format!("/r/{id}") || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    Server::parse(&url.origin().ascii_serialization()).ok()
}

/// The server to use: `flag`, else the setting, else [`DEFAULT_SERVER`]. `None` when the choice is
/// empty, which hides sending.
pub fn resolve(setting: Option<&str>, flag: Option<&str>) -> Result<Option<Server>, Error> {
    let address = flag.or(setting).unwrap_or(DEFAULT_SERVER).trim();
    if address.is_empty() {
        return Ok(None);
    }
    Server::parse(address).map(Some)
}

pub fn setting(db: &Db) -> Option<String> {
    db.setting(SERVER_SETTING)
        .inspect_err(|err| tracing::warn!(%err, "could not read the report server setting"))
        .ok()
        .flatten()
}

/// The server in effect for the window, from the settings alone. Blocking database read.
pub fn configured() -> Result<Option<Server>, Error> {
    let setting = crate::core::db().ok().and_then(|db| setting(&db));
    resolve(setting.as_deref(), None)
}

pub enum Error {
    Address(String),
    Network(String),
    Rejected(Vec<(String, String)>),
    RateLimited(Option<u64>),
    TooLarge,
    UnsupportedType,
    Status(u16),
    Answer,
    NotFound,
    Conflict,
    UnknownReport(String),
    Registry { path: PathBuf, err: io::Error },
    Internal(String),
}

impl Error {
    pub fn text(&self) -> Text {
        match self {
            Self::Address(address) => Text::new("ps.report.send.errors.address").arg("address", address),
            Self::Network(message) => Text::new("ps.report.send.errors.network").arg("error", message),
            Self::Rejected(rejections) => {
                Text::new("ps.report.send.errors.rejected").arg("fields", describe(rejections))
            }
            Self::RateLimited(Some(seconds)) => {
                Text::new("ps.report.send.errors.rate_limited.wait").arg("seconds", seconds)
            }
            Self::RateLimited(None) => Text::new("ps.report.send.errors.rate_limited.later"),
            Self::TooLarge => Text::new("ps.report.send.errors.too_large"),
            Self::UnsupportedType => Text::new("ps.report.send.errors.unsupported_type"),
            Self::Status(code) => Text::new("ps.report.send.errors.status").arg("status", code),
            Self::Answer => Text::new("ps.report.send.errors.answer"),
            Self::NotFound => Text::new("ps.report.send.errors.not_found"),
            Self::Conflict => Text::new("ps.report.send.errors.conflict"),
            Self::UnknownReport(id) => Text::new("ps.report.send.errors.unknown_report").arg("id", id),
            Self::Registry { path, err } => Text::new("ps.report.send.errors.registry")
                .arg("path", path.display())
                .arg("error", err),
            Self::Internal(message) => Text::verbatim(message),
        }
    }

    /// The whole chain, since `error sending request` alone does not say whether the server was down
    /// or the certificate failed.
    fn network(err: reqwest::Error) -> Self {
        let err = err.without_url();
        let mut message = err.to_string();
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            message.push_str(": ");
            message.push_str(&cause.to_string());
            source = cause.source();
        }
        Self::Network(message)
    }
}

fn describe(rejections: &[(String, String)]) -> String {
    let shown = rejections
        .iter()
        .take(MAX_REJECTIONS_SHOWN)
        .map(|(path, rule)| format!("{path}: {rule}"))
        .collect::<Vec<_>>()
        .join("; ");
    match rejections.len().checked_sub(MAX_REJECTIONS_SHOWN) {
        Some(more) if more > 0 => format!("{shown}; +{more}"),
        _ => shown,
    }
}

/// A field path or rule as the server named it, reduced to the characters those are made of so a
/// hostile server cannot put anything else in front of the player.
fn plain(part: &str, fallback: &str) -> String {
    let kept: String = part
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']' | '-' | ':' | ','))
        .take(MAX_REJECTION_PART)
        .collect();
    if kept.is_empty() { fallback.to_owned() } else { kept }
}

pub fn is_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('d' | 'p')) && id.len() == 33 && chars.all(is_lower_hex)
}

fn is_lower_hex(c: char) -> bool {
    matches!(c, '0'..='9' | 'a'..='f')
}

pub fn is_hex(value: &str, len: usize) -> bool {
    value.len() == len && value.chars().all(is_lower_hex)
}

/// What a created report is called, once checked.
#[derive(Clone)]
pub struct Sent {
    pub id: String,
    pub delete_code: String,
    pub url: String,
    pub fingerprint: String,
}

#[derive(Deserialize)]
struct Created {
    id: String,
    delete_code: String,
    url: String,
    fingerprint: String,
}

#[derive(Deserialize)]
struct Refused {
    rejections: Vec<Rejected>,
}

#[derive(Deserialize)]
struct Rejected {
    path: String,
    /// A word (`identity`) or a one-key object (`{"text": "path"}`), as the validator's `Rule` serializes.
    rule: serde_json::Value,
}

fn rule_text(rule: &serde_json::Value) -> String {
    match rule {
        serde_json::Value::String(word) => word.clone(),
        serde_json::Value::Object(parts) => parts
            .iter()
            .map(|(key, value)| format!("{key}:{}", value.as_str().unwrap_or("?")))
            .collect::<Vec<_>>()
            .join(","),
        _ => String::new(),
    }
}

fn client() -> Result<reqwest::Client, Error> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .redirect(redirect::Policy::none())
        .user_agent(concat!("decanter/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(Error::network)
}

async fn read_capped(mut response: Response) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(Error::network)? {
        if body.len() + chunk.len() > MAX_ANSWER {
            return Err(Error::Answer);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn retry_after(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|seconds| *seconds <= 7 * 24 * 3600)
}

fn refusal(response: &Response) -> Error {
    match response.status() {
        StatusCode::PAYLOAD_TOO_LARGE => Error::TooLarge,
        StatusCode::UNSUPPORTED_MEDIA_TYPE => Error::UnsupportedType,
        StatusCode::TOO_MANY_REQUESTS => Error::RateLimited(retry_after(response)),
        status => Error::Status(status.as_u16()),
    }
}

fn check_created(created: Created, product: Product, server: &Server) -> Result<Sent, Error> {
    let Created {
        id,
        delete_code,
        url,
        fingerprint,
    } = created;
    let url_ok = server_of(&url, &id).is_some_and(|origin| origin.origin() == server.origin());
    if is_id(&id)
        && id.starts_with(product.id_letter())
        && is_hex(&delete_code, 32)
        && is_hex(&fingerprint, 16)
        && url_ok
    {
        Ok(Sent {
            id,
            delete_code,
            url,
            fingerprint,
        })
    } else {
        Err(Error::Answer)
    }
}

/// `POST /v1/reports` with exactly the JSON the player previewed.
pub async fn send(built: &Built, server: &Server) -> Result<Sent, Error> {
    let response = client()?
        .post(server.endpoint("/v1/reports")?)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(built.json.clone())
        .send()
        .await
        .map_err(Error::network)?;
    let status = response.status();
    if status == StatusCode::UNPROCESSABLE_ENTITY {
        let refused: Refused = serde_json::from_slice(&read_capped(response).await?).map_err(|_| Error::Answer)?;
        return Err(Error::Rejected(
            refused
                .rejections
                .iter()
                .map(|rejected| (plain(&rejected.path, "report"), plain(&rule_text(&rejected.rule), "?")))
                .collect(),
        ));
    }
    if status != StatusCode::CREATED {
        return Err(refusal(&response));
    }
    let created: Created = serde_json::from_slice(&read_capped(response).await?).map_err(|_| Error::Answer)?;
    let sent = check_created(created, built.product, server)?;
    tracing::info!(id = sent.id, fingerprint = sent.fingerprint, "sent an error report");
    Ok(sent)
}

/// `PUT /v1/reports/<id>/attachment`: the private log, readable only by whoever maintains the server.
pub async fn attach_log(server: &Server, id: &str, delete_code: &str, bytes: Vec<u8>) -> Result<(), Error> {
    let size = bytes.len();
    let response = client()?
        .put(server.endpoint(&format!("/v1/reports/{id}/attachment"))?)
        .header("X-Delete-Code", delete_code)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(bytes)
        .send()
        .await
        .map_err(Error::network)?;
    match response.status() {
        StatusCode::NO_CONTENT => {
            tracing::info!(id, size, "attached a log to a sent report");
            Ok(())
        }
        StatusCode::NOT_FOUND => Err(Error::NotFound),
        StatusCode::CONFLICT => Err(Error::Conflict),
        _ => Err(refusal(&response)),
    }
}

/// `DELETE /v1/reports/<id>`. A 404 is `Ok(false)`: the server does not say whether the report is
/// gone or the code is wrong, and the code came from the player's own registry.
async fn delete_remote(server: &Server, id: &str, delete_code: &str) -> Result<bool, Error> {
    let response = client()?
        .delete(server.endpoint(&format!("/v1/reports/{id}"))?)
        .header("X-Delete-Code", delete_code)
        .send()
        .await
        .map_err(Error::network)?;
    match response.status() {
        StatusCode::NO_CONTENT => Ok(true),
        StatusCode::NOT_FOUND => Ok(false),
        _ => Err(refusal(&response)),
    }
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, Error> + Send + 'static) -> Result<T, Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| Error::Internal(err.to_string()))?
}

pub enum Submitted {
    /// The same failure on the same day was sent before; nothing left the machine.
    Duplicate(Entry),
    Sent {
        entry: Entry,
        /// Set when the code could not be written to the registry: the player still has it on screen.
        unrecorded: Option<Error>,
        /// `None` when no log was asked for.
        attached: Option<Result<(), Error>>,
    },
}

/// The whole send of one press: the duplicate check, the report, the registry line and, if `log` is
/// given, the private log. A failure of the log never undoes the report. Blocking file work runs on
/// the runtime's blocking pool, so this is safe to await from the UI thread.
pub async fn submit(built: &Built, server: &Server, log: Option<&Path>) -> Result<Submitted, Error> {
    let (fingerprint, day) = (built.fingerprint.clone(), built.date.clone());
    if let Some(earlier) = blocking(move || sent::find(&fingerprint, &day)).await? {
        return Ok(Submitted::Duplicate(earlier));
    }
    let created = send(built, server).await?;
    let entry = Entry {
        id: created.id,
        delete_code: created.delete_code,
        url: created.url,
        fingerprint: created.fingerprint,
        product: built.product,
        day: built.date.clone(),
    };
    let recorded = entry.clone();
    let unrecorded = blocking(move || sent::record(&recorded)).await.err();
    let attached = match log {
        Some(log) => {
            let log = log.to_owned();
            let bytes =
                blocking(move || super::attachment(&log).map_err(|err| Error::Internal(err.text().to_string()))).await;
            Some(match bytes {
                Ok(bytes) => attach_log(server, &entry.id, &entry.delete_code, bytes).await,
                Err(err) => Err(err),
            })
        }
        None => None,
    };
    Ok(Submitted::Sent {
        entry,
        unrecorded,
        attached,
    })
}

pub enum Deleted {
    Removed,
    /// The server no longer knew it (removed after its retention, or the code is wrong); the line is
    /// dropped from the registry all the same.
    Gone,
}

/// Deletes a report from the server that holds it, then from the registry.
pub async fn delete(id: &str) -> Result<Deleted, Error> {
    let wanted = id.to_owned();
    let entry = blocking(move || sent::get(&wanted))
        .await?
        .ok_or_else(|| Error::UnknownReport(id.to_owned()))?;
    let server = server_of(&entry.url, &entry.id).ok_or(Error::Answer)?;
    let removed = delete_remote(&server, &entry.id, &entry.delete_code).await?;
    let id = entry.id;
    blocking(move || sent::forget(&id)).await?;
    tracing::info!(removed, "deleted a sent error report");
    Ok(if removed { Deleted::Removed } else { Deleted::Gone })
}
