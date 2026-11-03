// SPDX-License-Identifier: GPL-3.0-or-later
//! GOG's per-game cloud storage on `cloudstorage.gog.com` and the exchange for the game-scoped
//! session it answers to. GOG documents neither; what is sent and read here is the behaviour
//! `docs/gog-cloud-saves-behaviour-spec.md` records from other clients, sections 1 and 2.

use core::time::Duration;

use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use decanter_core::secrets::Secret;
use reqwest::header::{ACCEPT, CONTENT_ENCODING, ETAG, USER_AGENT};
use reqwest::{Method, RequestBuilder, Url};
use serde::Deserialize;

use super::Error;
use super::auth::{GRANT_REFUSED, is_account_id, secret};
use super::http::{self, Call, Host, SESSION_REFUSED, Sessionless};
use super::stats::is_client_id;

/// Its answer is a token pair, so no part of the body may be quoted in an error.
const GAME_SESSION: Call = Call {
    name: "game session exchange",
    refused: GRANT_REFUSED,
    max_body: 64 << 10,
    has_secrets: true,
};

const LISTING: Call = Call {
    name: "cloud saves listing",
    refused: SESSION_REFUSED,
    max_body: 8 << 20,
    has_secrets: false,
};

/// GOG states no object size limit; this bounds what one download may hold in memory.
const DOWNLOAD: Call = Call {
    name: "cloud save download",
    refused: SESSION_REFUSED,
    max_body: 256 << 20,
    has_secrets: false,
};

/// The answers to these three are not read, so their cap is never reached.
const UPLOAD: Call = Call {
    name: "cloud save upload",
    refused: SESSION_REFUSED,
    max_body: 64 << 10,
    has_secrets: false,
};

const DELETE: Call = Call {
    name: "cloud save deletion",
    refused: SESSION_REFUSED,
    max_body: 64 << 10,
    has_secrets: false,
};

const COMMIT: Call = Call {
    name: "cloud saves commit",
    refused: SESSION_REFUSED,
    max_body: 64 << 10,
    has_secrets: false,
};

/// `dont_sync_marker/true` is the token other clients known to work carry; with it, this agent was
/// answered on every bucket call and no Galaxy agent is needed (spec Q2).
const AGENT: &str = concat!("decanter/", env!("CARGO_PKG_VERSION"), " dont_sync_marker/true");

/// The client's 30 seconds cover every small JSON round trip; an object of up to the download cap
/// needs longer on a slow link, so transfers alone get this instead.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);

const LOCAL_LAST_MODIFIED: &str = "X-Object-Meta-LocalLastModified";

/// A session scoped to one game build's Galaxy client. Any refresh token GOG adds to the answer is
/// left unread, so that it can never replace the account's own.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct GameSession {
    #[serde(deserialize_with = "secret")]
    pub access_token: Secret,
    /// Checked to be decimal digits, since it becomes the bucket's path segment.
    pub user_id: String,
}

/// One game's bucket, its ids checked once so that every object URL is built from safe segments.
pub struct Bucket {
    base: Url,
    game_token: Secret,
}

impl Bucket {
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a user id that is not decimal digits or a client id that is not
    /// ASCII letters and digits.
    #[inline]
    pub fn new(user_id: &str, client_id: &str, game_token: Secret) -> Result<Self, Error> {
        if !is_account_id(user_id) {
            return Err(Error::InvalidInput { what: "account id" });
        }
        if !is_client_id(client_id) {
            return Err(Error::InvalidInput { what: "game client id" });
        }
        let base = Url::parse(&Host::CloudStorage.url(&format!("/v1/{user_id}/{client_id}")))
            .ok()
            .ok_or(Error::InvalidInput {
                what: "cloud storage bucket",
            })?;
        Ok(Self { base, game_token })
    }

    /// Each `/`-separated segment of `name` is percent-encoded on its own, so a name can never
    /// carry an encoded separator nor step out of the bucket with `.` or `..`.
    fn object(&self, name: &str) -> Result<Url, Error> {
        let invalid = || Error::InvalidInput {
            what: "cloud object name",
        };
        if name.split('/').any(|segment| matches!(segment, "" | "." | "..")) {
            return Err(invalid());
        }
        let mut url = self.base.clone();
        url.path_segments_mut().map_err(|()| invalid())?.extend(name.split('/'));
        Ok(url)
    }

    fn authorized(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(self.game_token.expose()).header(USER_AGENT, AGENT)
    }
}

/// One entry of a bucket's listing.
#[non_exhaustive]
pub struct CloudObject {
    /// Never empty; still data from the service, to be checked before it names a local file.
    pub name: String,
    /// The md5, in lowercase hex, of the object as stored (compressed), when listed.
    pub hash: Option<String>,
    /// `None` when absent or not a timestamp this reads.
    pub last_modified: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    last_modified: Option<String>,
}

/// An object's body as the service sent it, and the time its uploader recorded for the file.
#[non_exhaustive]
pub struct Downloaded {
    pub body: Vec<u8>,
    pub local_last_modified: Option<DateTime<Utc>>,
}

/// RFC 3339 with any offset, or ISO 8601 with no zone at all, which is read as UTC.
fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|time| time.and_utc())
        })
}

impl Sessionless {
    /// Trades the account's `refresh_token` for a session of the build client `client_id`, without
    /// opening a new login session, so that the account's own refresh token stays valid.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a client id that cannot be one; [`Error::Rejected`] when GOG
    /// refuses the grant (400, 401 or 403); [`Error::Malformed`] for an answer with no access token
    /// or a user id that is not decimal digits; otherwise [`Error::Network`] or
    /// [`Error::Unavailable`].
    #[inline]
    pub async fn game_session(
        &self,
        client_id: &str,
        client_secret: &Secret,
        refresh_token: &Secret,
    ) -> Result<GameSession, Error> {
        if !is_client_id(client_id) {
            return Err(Error::InvalidInput { what: "game client id" });
        }
        let query = [
            ("client_id", client_id),
            ("client_secret", client_secret.expose()),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.expose()),
            ("without_new_session", "1"),
        ];
        let session: GameSession = self.get(&GAME_SESSION, Host::Auth, "/token", &query, None).await?;
        if session.access_token.is_empty() {
            return Err(Error::Malformed {
                call: GAME_SESSION.name,
                reason: "no access token".to_owned(),
            });
        }
        if !is_account_id(&session.user_id) {
            return Err(Error::Malformed {
                call: GAME_SESSION.name,
                reason: "user_id is not a numeric id".to_owned(),
            });
        }
        Ok(session)
    }

    /// Every object of `bucket`, tombstones included; `None` when GOG answers 404, which other
    /// clients read as a bucket nothing was ever saved to, although an empty bucket was seen to
    /// answer 200 with no entries (spec Q3). Entries with no name are
    /// left out.
    ///
    /// # Errors
    ///
    /// [`Error::Rejected`] when GOG refuses the game session; [`Error::Malformed`] for a listing
    /// past its 8 megabyte cap or of another shape; otherwise [`Error::Network`] or [`Error::Unavailable`].
    #[inline]
    pub async fn cloud_objects(&self, bucket: &Bucket) -> Result<Option<Vec<CloudObject>>, Error> {
        let request = bucket
            .authorized(self.request(Method::GET, bucket.base.clone()))
            .header(ACCEPT, "application/json");
        let response = match self.answer(&LISTING, request, Host::CloudStorage.name()).await {
            Err(Error::Unavailable { status: 404, .. }) => return Ok(None),
            answer => answer?,
        };
        let entries: Vec<Entry> = http::parse(&LISTING, &http::read(response, &LISTING).await?)?;
        let listed = entries.len();
        let objects: Vec<_> = entries
            .into_iter()
            .filter_map(|entry| {
                let name = entry.name.filter(|name| !name.is_empty())?;
                Some(CloudObject {
                    name,
                    hash: entry.hash.map(|hash| hash.to_ascii_lowercase()),
                    last_modified: entry.last_modified.as_deref().and_then(parse_time),
                })
            })
            .collect();
        let unnamed = listed.saturating_sub(objects.len());
        if unnamed > 0 {
            tracing::debug!(unnamed, "cloud saves listing has entries with no name, ignored");
        }
        Ok(Some(objects))
    }

    /// The stored body of `name`, as sent: reqwest here decodes no `Content-Encoding`, so a gzip
    /// object arrives compressed.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a name with an empty, `.` or `..` segment; [`Error::Malformed`]
    /// for a body past its 256 megabyte cap; the others as [`Sessionless::cloud_objects`], a 404 included.
    #[inline]
    pub async fn cloud_download(&self, bucket: &Bucket, name: &str) -> Result<Downloaded, Error> {
        let request = bucket
            .authorized(self.request(Method::GET, bucket.object(name)?))
            .timeout(TRANSFER_TIMEOUT);
        let response = self.answer(&DOWNLOAD, request, Host::CloudStorage.name()).await?;
        let local_last_modified = response
            .headers()
            .get(LOCAL_LAST_MODIFIED)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_time);
        let body = http::read(response, &DOWNLOAD).await?;
        Ok(Downloaded {
            body,
            local_last_modified,
        })
    }

    /// Stores `gzipped`, which must already be gzip, as `name`, recording `local_last_modified`
    /// (whole seconds, UTC) as the file's own time.
    ///
    /// # Errors
    ///
    /// As [`Sessionless::cloud_download`], without the size cap.
    #[inline]
    pub async fn cloud_upload(
        &self,
        bucket: &Bucket,
        name: &str,
        gzipped: Vec<u8>,
        local_last_modified: DateTime<Utc>,
    ) -> Result<(), Error> {
        let etag = format!("{:x}", md5::compute(&gzipped));
        let request = bucket
            .authorized(self.request(Method::PUT, bucket.object(name)?))
            .timeout(TRANSFER_TIMEOUT)
            .header(CONTENT_ENCODING, "gzip")
            .header(ETAG, etag)
            .header(
                LOCAL_LAST_MODIFIED,
                local_last_modified.to_rfc3339_opts(SecondsFormat::Secs, false),
            )
            .body(gzipped);
        self.answer(&UPLOAD, request, Host::CloudStorage.name()).await?;
        Ok(())
    }

    /// Removes `name` from the bucket.
    ///
    /// # Errors
    ///
    /// As [`Sessionless::cloud_download`], without the size cap.
    #[inline]
    pub async fn cloud_delete(&self, bucket: &Bucket, name: &str) -> Result<(), Error> {
        let request = bucket.authorized(self.request(Method::DELETE, bucket.object(name)?));
        self.answer(&DELETE, request, Host::CloudStorage.name()).await?;
        Ok(())
    }

    /// Closes a round of uploads and deletions. GOG accepts it and the listing does not depend on
    /// it; whether other clients need it to see the changes is untested (spec Q1).
    ///
    /// # Errors
    ///
    /// [`Error::Rejected`] when GOG refuses the game session; otherwise [`Error::Network`] or
    /// [`Error::Unavailable`].
    #[inline]
    pub async fn cloud_commit(&self, bucket: &Bucket) -> Result<(), Error> {
        let request = bucket.authorized(self.request(Method::POST, bucket.base.clone()));
        self.answer(&COMMIT, request, Host::CloudStorage.name()).await?;
        Ok(())
    }
}
