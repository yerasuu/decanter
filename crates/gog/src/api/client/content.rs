// SPDX-License-Identifier: GPL-3.0-or-later
//! GOG Galaxy's builds and depot v2 API: which files a game is made of and where to download each
//! piece. The chunk downloads themselves are `decanter-gog`'s install flow, not this client.
//!
//! Protocol details (CDN sharding, zlib'd manifests, secure-link templating) mirror gogdl, which
//! is what GOG's own tooling agrees with. GOG documents none of `content-system.gog.com`
//! officially (its [developer docs](https://docs.gog.com/) cover the publisher side only); the
//! community's [gogapidocs](https://gogapidocs.readthedocs.io/en/latest/galaxy.html) lists the
//! builds endpoint with `windows` and `osx` as its systems.

use std::collections::BTreeMap;
use std::io::Read as _;

use decanter_core::GameSystem;
use flate2::read::ZlibDecoder;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::Error;
use super::http::{self, Call, Host, SESSION_REFUSED, Sessionless};

const BUILDS: Call = Call {
    name: "content-system builds",
    refused: SESSION_REFUSED,
    max_body: 4 << 20,
    has_secrets: false,
};

/// The build manifest holds the game's Galaxy client secret.
const BUILD_MANIFEST: Call = Call {
    name: "build manifest",
    refused: SESSION_REFUSED,
    max_body: 16 << 20,
    has_secrets: true,
};

const BUILD_MANIFEST_INFLATED: usize = 64 << 20;

/// A depot lists every chunk of every file it holds, so the largest games reach tens of
/// megabytes once inflated.
const DEPOT_MANIFEST: Call = Call {
    name: "depot manifest",
    refused: &[],
    max_body: 64 << 20,
    has_secrets: false,
};

const DEPOT_MANIFEST_INFLATED: usize = 256 << 20;

/// Its URLs carry signed CDN tokens.
const SECURE_LINK: Call = Call {
    name: "secure link",
    refused: SESSION_REFUSED,
    max_body: 1 << 20,
    has_secrets: true,
};

/// A manifest id is an MD5 in hex, sometimes already split into its CDN path.
const MAX_MANIFEST_ID_LEN: usize = 128;

#[derive(Deserialize)]
struct BuildsAnswer {
    items: Vec<Build>,
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct Build {
    pub build_id: String,
    pub branch: Option<String>,
    pub link: String,
    /// GOG's own human label for this build, when the publisher set one: `"1.2.3"`, say. `None`
    /// is common; `build_id` is the only thing guaranteed to identify a build at all.
    pub version_name: Option<String>,
    pub date_published: Option<String>,
}

#[derive(Deserialize)]
#[non_exhaustive]
pub struct BuildMeta {
    /// GOG's own canonical install folder name for this game (what gogdl and Galaxy install
    /// under `Program Files\`): depot files' relative paths are flat, with no game-named prefix
    /// of their own.
    #[serde(rename = "installDirectory")]
    pub install_directory: String,
    pub depots: Vec<DepotMeta>,
    /// This build's Galaxy client id and secret, which scope a cloud-saves token to this game
    /// specifically, distinct from decanter's own login client.
    #[serde(rename = "clientId", default)]
    pub client_id: Option<String>,
    #[serde(rename = "clientSecret", default)]
    pub client_secret: Option<String>,
}

#[derive(Deserialize)]
#[non_exhaustive]
pub struct DepotMeta {
    #[serde(rename = "osBitness", default)]
    pub os_bitness: Option<Vec<String>>,
    pub manifest: String,
    /// Which product's content this depot holds.
    ///
    /// A build carries one depot per product the game covers (the base game and every DLC of
    /// it), so this is what says whose files these are. It also decides which product the chunk
    /// URLs have to be signed for: a depot's chunks live under its own product's path, and a link
    /// signed for a different one is refused.
    #[serde(rename = "productId", default)]
    pub product_id: Option<String>,
}

#[derive(Deserialize)]
struct DepotManifestRoot {
    depot: DepotManifestBody,
}

#[derive(Deserialize)]
struct DepotManifestBody {
    #[serde(default)]
    items: Vec<DepotItem>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum DepotItem {
    DepotFile(DepotFile),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[non_exhaustive]
pub struct DepotFile {
    pub path: String,
    #[serde(default)]
    pub chunks: Vec<DepotChunk>,
    /// `executable` marks a file that needs its mode to run, as
    /// [gogdl](https://github.com/Heroic-Games-Launcher/heroic-gogdl/blob/main/gogdl/dl/managers/task_executor.py)
    /// reads it; GOG doesn't document the field.
    #[serde(default)]
    pub flags: Vec<String>,
}

/// One slice of a depot file.
///
/// The uncompressed size and MD5 are what let a file be verified against this manifest without
/// any network access beyond the manifest itself: read the local file in `size`-sized slices, in
/// order, hash each, and compare against `md5`.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct DepotChunk {
    #[serde(rename = "compressedMd5")]
    pub compressed_md5: String,
    pub md5: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct SecureLinkAnswer {
    urls: Vec<SecureLinkEntry>,
}

/// One CDN endpoint chunk URLs can be built from.
#[derive(Deserialize)]
pub struct SecureLinkEntry {
    url: Option<String>,
    url_format: Option<String>,
    #[serde(default)]
    parameters: BTreeMap<String, serde_json::Value>,
}

impl SecureLinkEntry {
    /// The download URL of one chunk, mirroring gogdl's `_get_download_url_v2`: append the
    /// chunk's compressed-bytes hash (galaxy-sharded) onto the endpoint's base path, then
    /// substitute the resulting parameters into its `{placeholder}` URL template.
    #[inline]
    #[must_use]
    pub fn chunk_url(&self, compressed_md5: &str) -> Option<String> {
        let template = self.url_format.as_deref().or(self.url.as_deref())?;
        let chunk_path = format!(
            "{}/{}",
            self.parameters
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
            galaxy_path(compressed_md5)
        );
        let mut url = template.to_owned();
        for (key, value) in &self.parameters {
            // `path` takes the chunk's own value built above, not the base path the answer
            // carried; the rest mix strings (base_url, token) with bare JSON numbers (expires_at,
            // dirs), and both go into the template.
            let replacement = if key == "path" {
                Some(chunk_path.clone())
            } else {
                value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_i64().map(|number| number.to_string()))
                    .or_else(|| value.as_u64().map(|number| number.to_string()))
            };
            if let Some(text) = replacement {
                url = url.replace(&format!("{{{key}}}"), &text);
            }
        }
        Some(url)
    }
}

impl Sessionless {
    /// The `system` builds of `game_id`, newest and mainline first in GOG's own order; empty
    /// when it has none.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for an id that is not positive, [`Error::Rejected`] when GOG
    /// refuses `access_token`, which this client does not renew; the others when GOG cannot be
    /// asked or answers something unreadable.
    #[inline]
    pub async fn builds(&self, access_token: &str, game_id: i64, system: GameSystem) -> Result<Vec<Build>, Error> {
        if game_id <= 0 {
            return Err(Error::InvalidInput { what: "product id" });
        }
        let path = format!("/products/{game_id}/os/{}/builds", os_segment(system));
        let answer: BuildsAnswer = self
            .get(
                &BUILDS,
                Host::ContentSystem,
                &path,
                &[("generation", "2")],
                Some(access_token),
            )
            .await?;
        Ok(answer.items)
    }

    /// The build manifest a [`Build::link`] points at.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] for a link that is not an `https` URL on a GOG host, or a manifest
    /// that is not zlib'd JSON within its size caps; the others as [`Sessionless::builds`].
    #[inline]
    pub async fn build_meta(&self, access_token: &str, link: &str) -> Result<BuildMeta, Error> {
        let host = gog_host(link).ok_or_else(|| Error::Malformed {
            call: BUILD_MANIFEST.name,
            reason: "link is not an https URL on a GOG host".to_owned(),
        })?;
        let compressed = self
            .get_bytes(&BUILD_MANIFEST, link, &host, &[], Some(access_token))
            .await?;
        inflated(&BUILD_MANIFEST, &compressed, BUILD_MANIFEST_INFLATED)
    }

    /// Every file of one depot, from its manifest on the public CDN.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a manifest id that is not hex digits, optionally split by
    /// `/`; [`Error::Malformed`] for a manifest that is not zlib'd JSON within its size caps; the
    /// others when the CDN cannot be asked.
    #[inline]
    pub async fn depot_files(&self, manifest: &str) -> Result<Vec<DepotFile>, Error> {
        let is_manifest_id = !manifest.is_empty()
            && manifest.len() <= MAX_MANIFEST_ID_LEN
            && manifest.bytes().all(|byte| byte.is_ascii_hexdigit() || byte == b'/');
        if !is_manifest_id {
            return Err(Error::InvalidInput {
                what: "depot manifest id",
            });
        }
        let url = Host::Cdn.url(&format!("/content-system/v2/meta/{}", galaxy_path(manifest)));
        let compressed = self
            .get_bytes(&DEPOT_MANIFEST, &url, Host::Cdn.name(), &[], None)
            .await?;
        let root: DepotManifestRoot = inflated(&DEPOT_MANIFEST, &compressed, DEPOT_MANIFEST_INFLATED)?;
        Ok(root
            .depot
            .items
            .into_iter()
            .filter_map(|item| match item {
                DepotItem::DepotFile(file) => Some(file),
                DepotItem::Other => None,
            })
            .collect())
    }

    /// The CDN endpoints chunk URLs of `product_id` can be built from, seeded with `path` as
    /// the base path each chunk's own hash is appended to.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a `path` with a character outside ASCII letters, digits and
    /// `/ _ - . ~`; the others as [`Sessionless::builds`].
    #[inline]
    pub async fn secure_link(
        &self,
        access_token: &str,
        product_id: i64,
        path: &str,
    ) -> Result<Vec<SecureLinkEntry>, Error> {
        if product_id <= 0 {
            return Err(Error::InvalidInput { what: "product id" });
        }
        // `path` goes into the query raw, as gogdl and decanter always sent it (`path=/`, not
        // `path=%2F`), so only characters that mean the same raw in a query get through.
        let is_raw_safe = path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.~".contains(&byte));
        if !is_raw_safe {
            return Err(Error::InvalidInput {
                what: "secure link path",
            });
        }
        let url = Host::ContentSystem.url(&format!(
            "/products/{product_id}/secure_link?_version=2&generation=2&path={path}"
        ));
        let body = self
            .get_bytes(&SECURE_LINK, &url, Host::ContentSystem.name(), &[], Some(access_token))
            .await?;
        let answer: SecureLinkAnswer = http::parse(&SECURE_LINK, &body)?;
        Ok(answer.urls)
    }
}

/// The content system's name for `system`.
const fn os_segment(system: GameSystem) -> &'static str {
    match system {
        GameSystem::Windows => "windows",
        GameSystem::MacOs => "osx",
    }
}

/// GOG's CDN shards content by the first 4 hex chars of its hash: `ab12cd...` ->
/// `ab/12/ab12cd...`.
fn galaxy_path(hash: &str) -> String {
    match (hash.get(..2), hash.get(2..4)) {
        (Some(first), Some(second)) if !hash.contains('/') => format!("{first}/{second}/{hash}"),
        _ => hash.to_owned(),
    }
}

/// The host of `link` when it is an `https` URL on `gog.com` or under it, with no port or
/// credentials: the access token rides along to it.
fn gog_host(link: &str) -> Option<String> {
    let url = url::Url::parse(link).ok()?;
    let host = url.host_str()?;
    let is_gog = url.scheme() == "https"
        && (host == "gog.com" || host.ends_with(".gog.com"))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    is_gog.then(|| host.to_owned())
}

/// Reads zlib'd `compressed` as `T`, refused once it inflates past `max` bytes.
fn inflated<T>(call: &Call, compressed: &[u8], max: usize) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    let mut body = Vec::new();
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    ZlibDecoder::new(compressed)
        .take(limit)
        .read_to_end(&mut body)
        .map_err(|err| Error::Malformed {
            call: call.name,
            reason: format!("not zlib data: {err}"),
        })?;
    if body.len() > max {
        return Err(Error::Malformed {
            call: call.name,
            reason: format!("inflates past {max} bytes"),
        });
    }
    http::parse(call, &body)
}
