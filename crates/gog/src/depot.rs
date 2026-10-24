//! A client for GOG Galaxy's builds/depot v2 API — the endpoints that say
//! which files a game is made of and where to download each piece.
//!
//! Runs entirely in-process: no plugin, no external registry. Protocol
//! details (CDN sharding, zlib'd manifests, secure-link templating)
//! mirror gogdl, which is what GOG's own tooling agrees with.
//!
//! GOG documents none of `content-system.gog.com` officially (its
//! [developer docs](https://docs.gog.com/) cover the publisher side only);
//! the community's [gogapidocs](https://gogapidocs.readthedocs.io/en/latest/galaxy.html)
//! lists the builds endpoint with `windows` and `osx` as its systems.

use std::io::Read;

use decanter_core::GameSystem;
use serde::Deserialize;

const CONTENT_SYSTEM: &str = "https://content-system.gog.com";
const CDN: &str = "https://gog-cdn-fastly.gog.com";

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to decompress response: {0}")]
    Decompress(#[from] std::io::Error),
    #[error("failed to parse response: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("no {0} builds available for this product")]
    NoBuilds(GameSystem),
}

/// GOG's CDN shards content by the first 4 hex chars of its hash:
/// `ab12cd...` -> `ab/12/ab12cd...`.
fn galaxy_path(hash: &str) -> String {
    if hash.contains('/') || hash.len() < 4 {
        hash.to_string()
    } else {
        format!("{}/{}/{}", &hash[0..2], &hash[2..4], hash)
    }
}

fn zlib_decompress(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut decoder = flate2::read::ZlibDecoder::new(bytes);
    let mut out = Vec::new();
    crate::achv_try!(decoder.read_to_end(&mut out));
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct BuildsResponse {
    total_count: u64,
    items: Vec<Build>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Build {
    pub build_id: String,
    pub branch: Option<String>,
    pub link: String,
    /// GOG's own human label for this build, when the publisher set one —
    /// `"1.2.3"`, say. `None` is common; `build_id` is the only thing
    /// guaranteed to identify a build at all.
    pub version_name: Option<String>,
    pub date_published: Option<String>,
}

/// The content system's name for `system`.
const fn os_segment(system: GameSystem) -> &'static str {
    match system {
        GameSystem::Windows => "windows",
        GameSystem::MacOs => "osx",
    }
}

/// Lists available `system` builds for `game_id`, newest/mainline first per
/// GOG's own ordering.
pub async fn get_builds(
    http: &reqwest::Client,
    access_token: &str,
    game_id: i64,
    system: GameSystem,
) -> Result<Vec<Build>> {
    let url = format!(
        "{CONTENT_SYSTEM}/products/{game_id}/os/{}/builds?generation=2",
        os_segment(system)
    );
    let sent = crate::achv_try!(http.get(&url).bearer_auth(access_token).send().await);
    let response = crate::achv_try!(sent.error_for_status());
    let parsed: BuildsResponse = crate::achv_try!(response.json().await);
    if parsed.total_count == 0 {
        return Err(Error::NoBuilds(system));
    }
    tracing::debug!(game_id, %system, build_count = parsed.items.len(), "fetched gog builds");
    Ok(parsed.items)
}

/// Mainline over any named branch (beta, etc.), falling back to whatever
/// GOG listed first — same selection gogdl makes.
pub fn select_build(builds: &[Build]) -> Option<&Build> {
    select_build_for_branch(builds, None)
}

/// The build for a specific branch — `None` for mainline, same as
/// [`select_build`] but able to land on a beta deliberately instead of
/// only ever falling back to it.
///
/// `None` back means the branch named isn't (or is no longer) offered —
/// distinct from [`select_build`]'s fallback, because silently landing on
/// a different branch than the one asked for is exactly the kind of
/// surprise a beta opt-in has to not produce.
pub fn select_build_for_branch<'a>(builds: &'a [Build], branch: Option<&str>) -> Option<&'a Build> {
    match branch {
        None => builds
            .iter()
            .find(|build| build.branch.is_none())
            .or_else(|| builds.first()),
        Some(name) => builds.iter().find(|build| build.branch.as_deref() == Some(name)),
    }
}

/// One exact build by id, regardless of branch — a manual rollback to a
/// specific historical build rather than "whichever build a branch
/// currently points at".
pub fn select_build_by_id<'a>(builds: &'a [Build], build_id: &str) -> Option<&'a Build> {
    builds.iter().find(|build| build.build_id == build_id)
}

#[derive(Debug, Deserialize)]
pub struct BuildMeta {
    /// GOG's own canonical install folder name for this game (what
    /// gogdl/Galaxy install under `Program Files\`) — depot files'
    /// relative paths are flat, with no game-named prefix of their own.
    #[serde(rename = "installDirectory")]
    pub install_directory: String,
    pub depots: Vec<DepotMeta>,
    /// This build's Galaxy client id/secret — scopes a cloud-saves token to
    /// this game specifically, distinct from decanter's own login client.
    #[serde(rename = "clientId", default)]
    pub client_id: Option<String>,
    #[serde(rename = "clientSecret", default)]
    pub client_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DepotMeta {
    #[serde(rename = "osBitness", default)]
    pub os_bitness: Option<Vec<String>>,
    pub manifest: String,
    /// Which product's content this depot holds.
    ///
    /// A build carries one depot per product the game covers — the base
    /// game and every DLC of it — so this is what says whose files these
    /// are. It also decides which product the chunk URLs have to be signed
    /// for: a depot's chunks live under its own product's path, and a link
    /// signed for a different one is refused.
    #[serde(rename = "productId", default)]
    pub product_id: Option<String>,
}

impl DepotMeta {
    /// A build lists only its own system's depots, so only Windows', which
    /// can be marked by bitness, are told apart.
    fn is_compatible(&self, system: GameSystem) -> bool {
        match system {
            GameSystem::Windows => self
                .os_bitness
                .as_ref()
                .is_none_or(|bitness| bitness.iter().any(|b| b == "64" || b == "32")),
            GameSystem::MacOs => true,
        }
    }
}

/// Fetches and parses the build manifest a `Build.link` points at
/// (zlib-compressed JSON, same as the per-depot manifest below).
pub async fn get_build_meta(http: &reqwest::Client, access_token: &str, link: &str) -> Result<BuildMeta> {
    let sent = crate::achv_try!(http.get(link).bearer_auth(access_token).send().await);
    let response = crate::achv_try!(sent.error_for_status());
    let bytes = crate::achv_try!(response.bytes().await);
    let decompressed = crate::achv_try!(zlib_decompress(&bytes));
    let meta: BuildMeta = crate::achv_try!(serde_json::from_slice(&decompressed));
    tracing::debug!(depot_count = meta.depots.len(), "fetched gog build manifest");
    Ok(meta)
}

/// Every depot in this build belonging to `product_id`.
///
/// All of them, not the first: a product's content is regularly split
/// across more than one depot, and taking only the first installs a
/// fraction of the game. And only its own — a build for a game with DLC
/// lists the DLC's depots too, and those belong to whoever installs the
/// DLC, not to the base game.
pub fn select_depots(meta: &BuildMeta, product_id: i64, system: GameSystem) -> Vec<&DepotMeta> {
    let wanted = product_id.to_string();
    meta.depots
        .iter()
        .filter(|depot| depot.is_compatible(system))
        .filter(|depot| depot.product_id.as_deref() == Some(wanted.as_str()))
        .collect()
}

#[derive(Debug, Deserialize)]
struct DepotManifestRoot {
    depot: DepotManifestBody,
}

#[derive(Debug, Deserialize)]
struct DepotManifestBody {
    #[serde(default)]
    items: Vec<DepotItem>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum DepotItem {
    DepotFile(DepotFile),
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub struct DepotFile {
    pub path: String,
    #[serde(default)]
    pub chunks: Vec<DepotChunk>,
}

/// The uncompressed size and MD5 are what let a file be verified against
/// this manifest without any network access beyond the manifest itself:
/// read the local file in `size`-sized slices, in order, hash each, and
/// compare against `md5`.
#[derive(Debug, Deserialize)]
pub struct DepotChunk {
    #[serde(rename = "compressedMd5")]
    pub compressed_md5: String,
    pub md5: String,
    pub size: u64,
}

/// Fetches and parses one depot's file manifest — the CDN is public
/// content, no auth needed (same as GamesDB).
pub async fn get_depot_files(http: &reqwest::Client, manifest_hash: &str) -> Result<Vec<DepotFile>> {
    let url = format!("{CDN}/content-system/v2/meta/{}", galaxy_path(manifest_hash));
    let sent = crate::achv_try!(http.get(&url).send().await);
    let response = crate::achv_try!(sent.error_for_status());
    let bytes = crate::achv_try!(response.bytes().await);
    let decompressed = crate::achv_try!(zlib_decompress(&bytes));
    let root: DepotManifestRoot = crate::achv_try!(serde_json::from_slice(&decompressed));
    let files: Vec<DepotFile> = root
        .depot
        .items
        .into_iter()
        .filter_map(|item| match item {
            DepotItem::DepotFile(file) => Some(file),
            DepotItem::Other => None,
        })
        .collect();
    tracing::debug!(manifest_hash, file_count = files.len(), "fetched gog depot manifest");
    Ok(files)
}

#[derive(Debug, Deserialize)]
struct SecureLinkResponse {
    urls: Vec<SecureLinkEntry>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SecureLinkEntry {
    url: Option<String>,
    #[serde(rename = "url_format")]
    url_format: Option<String>,
    #[serde(default)]
    parameters: std::collections::HashMap<String, serde_json::Value>,
}

/// Requests the CDN endpoints usable to build chunk-download URLs for this
/// product. `path` is the base path seeded before appending each chunk's
/// own hash.
pub async fn get_secure_link(
    http: &reqwest::Client,
    access_token: &str,
    game_id: i64,
    path: &str,
) -> Result<Vec<SecureLinkEntry>> {
    let url = format!("{CONTENT_SYSTEM}/products/{game_id}/secure_link?_version=2&generation=2&path={path}");
    let sent = crate::achv_try!(http.get(&url).bearer_auth(access_token).send().await);
    let response = crate::achv_try!(sent.error_for_status());
    let parsed: SecureLinkResponse = crate::achv_try!(response.json().await);
    tracing::debug!(
        game_id,
        endpoint_count = parsed.urls.len(),
        "fetched gog secure link endpoints"
    );
    Ok(parsed.urls)
}

/// Builds the final downloadable URL for one chunk, mirroring gogdl's
/// `_get_download_url_v2`: append the chunk's compressed-bytes hash
/// (galaxy-sharded) onto the endpoint's base path, then substitute the
/// resulting parameters into its `{placeholder}` URL template.
pub fn build_chunk_url(entry: &SecureLinkEntry, compressed_md5: &str) -> Option<String> {
    let Some(template) = entry.url_format.as_deref().or(entry.url.as_deref()) else {
        return None;
    };
    let appended_path = format!(
        "{}/{}",
        entry
            .parameters
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or_default(),
        galaxy_path(compressed_md5)
    );

    let mut url = template.to_string();
    for (key, value) in &entry.parameters {
        // `path` is substituted with the chunk-specific value built above,
        // not the base path the response carried — no need to clone the
        // whole map just to override one key before this loop.
        let substituted = if key == "path" {
            Some(appended_path.clone())
        } else {
            // GOG's secure_link parameters mix strings (base_url, token)
            // with bare JSON numbers (expires_at, dirs) — both need
            // substituting into the `{placeholder}` template, so `as_str()`
            // alone isn't enough.
            value
                .as_str()
                .map(str::to_string)
                .or_else(|| value.as_i64().map(|n| n.to_string()))
                .or_else(|| value.as_u64().map(|n| n.to_string()))
        };
        if let Some(value) = substituted {
            url = url.replace(&format!("{{{key}}}"), &value);
        }
    }
    Some(url)
}
