//! Syncing a save folder against GOG's undocumented Cloud Saves API
//! (`cloudstorage.gog.com`). GOG publishes no docs for it; this is ported
//! from `heroic-gogdl` (`gogdl/saves.py`, `gogdl/auth.py`, GPL-3.0) — the
//! backend Heroic Games Launcher already ships for the same problem.
//!
//! v1 scope: syncs a manually-chosen local folder against a game's whole
//! cloud bucket, flat by relative path. The real Galaxy client appears to
//! namespace files under some per-location "dirname" inside the bucket
//! (`saves.py` filters listings by `name.startswith(cloud_save_dir_name)`),
//! but that naming convention couldn't be confirmed without reverse
//! engineering Heroic's closed-off Electron side. Skipping the filter
//! means anything a real Galaxy client already synced there still shows
//! up and gets synced too — we just don't replicate its internal
//! sub-foldering.
//!
//! A few `clippy::restriction` lints enabled workspace-wide are left
//! unaddressed in this file, each noted at its site rather than silenced:
//! `clippy::implicit_return` directly contradicts `clippy::all`'s
//! `needless_return` on the same tail expressions, so no code satisfies
//! both; `clippy::large_stack_frames` fires on ordinary async fn state
//! machines under `clippy.toml`'s 512-byte threshold; `clippy::single_call_fn`
//! flags this module's whole function-per-step decomposition style, which
//! is the readability choice, not an accident.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use decanter_core::platform;

use crate::api::client::Sessionless;
use crate::depot;

/// Base URL for GOG's Cloud Saves storage API.
const CLOUDSTORAGE: &str = "https://cloudstorage.gog.com";
/// Base URL for GOG's OAuth token endpoint.
const AUTH: &str = "https://auth.gog.com";

/// Cloud Saves marks a deleted file by uploading empty gzip content
/// instead of removing the listing entry — this is that content's md5,
/// filtered out of every listing this module reads.
const TOMBSTONE_MD5: &str = "aadd86936a80ee8a369579c3926f1b3c";

pub type Result<T> = std::result::Result<T, anyhow::Error>;

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CloudFile {
    pub relative_path: String,
    pub md5: String,
    pub last_modified: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    Upload,
    Download,
    /// Both sides changed since the last successful sync — nothing was
    /// moved. The caller has to pick a direction and retry with `force`.
    Conflict,
    None,
}

/// Skips classification and moves everything one direction — what
/// "subir/descargar de todos modos" means after a [`SyncAction::Conflict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForceDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SyncOutcome {
    pub action: SyncAction,
    pub uploaded: usize,
    pub downloaded: usize,
    /// Unix seconds to record as this game's last successful sync — only
    /// meaningful when `action` isn't `Conflict`, since a conflict leaves
    /// both sides exactly as they were.
    pub last_sync: i64,
}

/// A cloud-saves session scoped to one game's Galaxy client id, exchanged
/// from decanter's own login session — see [`game_token`].
struct GameToken {
    /// Bearer token for this game's cloud-saves session.
    access_token: String,
    /// GOG account id the session belongs to.
    user_id: String,
    /// The game build's own Galaxy client id — buckets are keyed by it.
    client_id: String,
}

/// This build's Galaxy client credentials, as resolved by [`resolve_build_client`].
pub(crate) struct BuildClient {
    /// The build's Galaxy client id.
    pub client_id: String,
    /// The build's Galaxy client secret.
    pub client_secret: String,
}

/// This build's Galaxy client id/secret, resolved off the currently
/// selected build for `game_id` — shared by [`game_token`] (which exchanges
/// it for a cloud-saves session) and save-location detection (which only
/// needs the id, to look up `remote-config.gog.com`).
pub(crate) async fn resolve_build_client(access_token: &str, game_id: i64) -> Result<BuildClient> {
    let http = match Sessionless::new() {
        Ok(http) => http,
        Err(err) => return Err(anyhow::Error::new(err).context("building the GOG HTTP client")),
    };
    let builds = match depot::get_builds(&http, access_token, game_id, platform::game_system()).await {
        Ok(builds) => builds,
        Err(err) => {
            tracing::error!(game_id, error = %err, "gog builds lookup failed, aborting cloud saves client resolution");
            return Err(err.context("GOG builds lookup failed"));
        }
    };
    let build = match depot::select_build(&builds) {
        Some(build) => build,
        None => return Err(anyhow::anyhow!("no builds available for GOG game {game_id}")),
    };
    let meta = match http.build_meta(access_token, &build.link).await {
        Ok(meta) => meta,
        Err(err) => {
            tracing::error!(game_id, error = %err, "gog build manifest fetch failed, aborting cloud saves client resolution");
            return Err(anyhow::Error::new(err).context("GOG build manifest fetch failed"));
        }
    };
    let client_id = match meta.client_id {
        Some(client_id) => client_id,
        None => return Err(anyhow::anyhow!("this game's build has no cloud saves client id")),
    };
    let client_secret = match meta.client_secret {
        Some(client_secret) => client_secret,
        None => return Err(anyhow::anyhow!("this game's build has no cloud saves client secret")),
    };
    tracing::debug!(game_id, client_id, "resolved gog cloud saves client");
    Ok(BuildClient {
        client_id,
        client_secret,
    })
}

/// A save file found on disk under the sync root, before it's classified
/// against the cloud listing.
struct LocalFile {
    /// Path relative to the sync root, forward-slash separated.
    relative_path: String,
    /// Last-modified time read from filesystem metadata.
    modified: DateTime<Utc>,
}

/// What [`classify`] decided to move, split by direction.
#[derive(Debug, Default)]
struct Classification {
    /// Relative paths to push to the cloud.
    upload: Vec<String>,
    /// Relative paths to pull from the cloud.
    download: Vec<String>,
    /// Local files to remove because the cloud no longer has them.
    delete_local: Vec<String>,
    /// Cloud files to remove because the local side no longer has them.
    delete_remote: Vec<String>,
}

/// Runs a full sync cycle: lists both sides, decides what moves, moves it,
/// and commits. `force` skips straight to one direction — how the caller
/// resolves a previous [`SyncAction::Conflict`].
pub async fn sync(
    account_id: &str,
    product_id: &str,
    local_root: &Path,
    last_sync: Option<i64>,
    force: Option<ForceDirection>,
) -> Result<SyncOutcome> {
    let game_id: i64 = match product_id.parse() {
        Ok(game_id) => game_id,
        Err(err) => return Err(anyhow::Error::new(err).context("GOG product_id must be numeric")),
    };
    tracing::info!(product_id, local_root = %local_root.display(), "starting cloud saves sync");
    if let Err(err) = tokio::fs::create_dir_all(local_root).await {
        return Err(anyhow::Error::new(err).context(format!("creating {}", local_root.display())));
    }

    let token = match game_token(account_id, game_id).await {
        Ok(token) => token,
        Err(err) => return Err(err),
    };
    let http = crate::http_client();

    let local_files = match scan_local_files(local_root) {
        Ok(local_files) => local_files,
        Err(err) => return Err(err),
    };
    let cloud_files = match list_files(&http, &token).await {
        Ok(cloud_files) => cloud_files,
        Err(err) => return Err(err),
    };
    let (action, plan) = classify(&local_files, &cloud_files, last_sync, force);

    let mut uploaded = 0_usize;
    let mut downloaded = 0_usize;

    match action {
        SyncAction::Upload => {
            for relative_path in &plan.upload {
                if let Err(err) = upload_file(&http, &token, local_root, relative_path).await {
                    return Err(err);
                }
                uploaded = uploaded.saturating_add(1);
            }
            for relative_path in &plan.delete_remote {
                if let Err(err) = delete_file(&http, &token, relative_path).await {
                    return Err(err);
                }
            }
            if let Err(err) = commit(&http, &token).await {
                return Err(err);
            }
        }
        SyncAction::Download => {
            let by_path: HashMap<&str, &CloudFile> = cloud_files
                .iter()
                .map(|file| (file.relative_path.as_str(), file))
                .collect();
            for relative_path in &plan.download {
                if let Some(file) = by_path.get(relative_path.as_str()) {
                    if let Err(err) = download_file(&http, &token, local_root, file).await {
                        return Err(err);
                    }
                    downloaded = downloaded.saturating_add(1);
                }
            }
            for relative_path in &plan.delete_local {
                let path = local_root.join(relative_path);
                if let Err(err) = tokio::fs::remove_file(&path).await {
                    tracing::warn!(path = %path.display(), error = %err, "couldn't remove local save file the cloud no longer has");
                }
            }
            if let Err(err) = commit(&http, &token).await {
                return Err(err);
            }
        }
        SyncAction::Conflict | SyncAction::None => {}
    }

    let now = i64::try_from(
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX);
    tracing::info!(product_id, ?action, uploaded, downloaded, "cloud saves sync completed");
    Ok(SyncOutcome {
        action,
        uploaded,
        downloaded,
        last_sync: if action == SyncAction::Conflict {
            last_sync.unwrap_or(0)
        } else {
            now
        },
    })
}

/// Decides what to move. Pure and side-effect-free so it can be tested
/// without a network — see the `tests` module below.
fn classify(
    local: &[LocalFile],
    cloud: &[CloudFile],
    last_sync: Option<i64>,
    force: Option<ForceDirection>,
) -> (SyncAction, Classification) {
    let local_paths: HashSet<&str> = local.iter().map(|file| file.relative_path.as_str()).collect();
    let cloud_paths: HashSet<&str> = cloud.iter().map(|file| file.relative_path.as_str()).collect();

    if let Some(direction) = force {
        return match direction {
            ForceDirection::Upload => (
                SyncAction::Upload,
                Classification {
                    upload: local.iter().map(|file| file.relative_path.clone()).collect(),
                    delete_remote: cloud
                        .iter()
                        .map(|file| file.relative_path.clone())
                        .filter(|path| !local_paths.contains(path.as_str()))
                        .collect(),
                    ..Default::default()
                },
            ),
            ForceDirection::Download => (
                SyncAction::Download,
                Classification {
                    download: cloud.iter().map(|file| file.relative_path.clone()).collect(),
                    delete_local: local
                        .iter()
                        .map(|file| file.relative_path.clone())
                        .filter(|path| !cloud_paths.contains(path.as_str()))
                        .collect(),
                    ..Default::default()
                },
            ),
        };
    }

    // Either side starting from nothing is unambiguous — mirrors what
    // upstream does before it ever builds a classifier.
    if !local.is_empty() && cloud.is_empty() {
        return (
            SyncAction::Upload,
            Classification {
                upload: local.iter().map(|file| file.relative_path.clone()).collect(),
                ..Default::default()
            },
        );
    }
    if local.is_empty() && !cloud.is_empty() {
        return (
            SyncAction::Download,
            Classification {
                download: cloud.iter().map(|file| file.relative_path.clone()).collect(),
                ..Default::default()
            },
        );
    }
    if local.is_empty() && cloud.is_empty() {
        return (SyncAction::None, Classification::default());
    }

    let threshold = last_sync.unwrap_or(0);
    let updated_local: Vec<&LocalFile> = local
        .iter()
        .filter(|file| file.modified.timestamp() > threshold)
        .collect();
    let updated_cloud: Vec<&CloudFile> = cloud
        .iter()
        .filter(|file| file.last_modified.timestamp() > threshold)
        .collect();

    // A file that sits on only one side and hasn't changed there since the
    // last sync wasn't just never synced -- it was deleted on the other
    // side. Checked so a lone deletion (nothing else touched) still
    // propagates instead of leaving `action` at `None` forever.
    let cloud_only_stale = cloud
        .iter()
        .any(|file| file.last_modified.timestamp() <= threshold && !local_paths.contains(file.relative_path.as_str()));
    let local_only_stale = local
        .iter()
        .any(|file| file.modified.timestamp() <= threshold && !cloud_paths.contains(file.relative_path.as_str()));

    let action = match (updated_local.is_empty(), updated_cloud.is_empty()) {
        (true, false) => SyncAction::Download,
        (false, true) => SyncAction::Upload,
        (true, true) if cloud_only_stale && local_only_stale => SyncAction::Conflict,
        (true, true) if cloud_only_stale => SyncAction::Upload,
        (true, true) if local_only_stale => SyncAction::Download,
        (true, true) => SyncAction::None,
        (false, false) => SyncAction::Conflict,
    };

    let mut plan = Classification::default();
    match action {
        SyncAction::Upload => {
            plan.upload = updated_local.iter().map(|file| file.relative_path.clone()).collect();
            plan.delete_remote = cloud
                .iter()
                .map(|file| file.relative_path.clone())
                .filter(|path| !local_paths.contains(path.as_str()))
                .collect();
        }
        SyncAction::Download => {
            plan.download = updated_cloud.iter().map(|file| file.relative_path.clone()).collect();
            plan.delete_local = local
                .iter()
                .map(|file| file.relative_path.clone())
                .filter(|path| !cloud_paths.contains(path.as_str()))
                .collect();
        }
        SyncAction::Conflict | SyncAction::None => {}
    }

    (action, plan)
}

/// Fetches this build's Galaxy client id/secret and exchanges decanter's
/// own refresh token for a session scoped to it — Cloud Saves buckets are
/// keyed by a game's own client id, not decanter's login client.
async fn game_token(account_id: &str, game_id: i64) -> Result<GameToken> {
    let http = crate::http_client();
    let (access_token, refresh_token) = match crate::with_session(account_id, |client| {
        let token = client.token.borrow();
        Ok((token.access_token.clone(), token.refresh_token.clone()))
    })
    .await
    {
        Ok(tokens) => tokens,
        Err(err) => return Err(err),
    };

    let BuildClient {
        client_id,
        client_secret,
    } = match resolve_build_client(&access_token, game_id).await {
        Ok(client) => client,
        Err(err) => return Err(err),
    };

    #[derive(serde::Deserialize)]
    struct TokenResponse {
        access_token: String,
        user_id: String,
    }

    let request = http.get(format!(
        "{AUTH}/token?client_id={client_id}&client_secret={client_secret}&grant_type=refresh_token&refresh_token={refresh_token}&without_new_session=1"
    ));
    // The URL carries the client secret and the refresh token, and reqwest puts it in its errors.
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            let err = err.without_url();
            tracing::error!(game_id, error = %err, "gog cloud-saves token request failed, aborting sync");
            return Err(err.into());
        }
    };
    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(err) => {
            let err = err.without_url();
            tracing::error!(game_id, error = %err, "gog refused a cloud-saves session, aborting sync");
            return Err(anyhow::Error::new(err).context("GOG refused a cloud-saves session for this game"));
        }
    };
    let response: TokenResponse = match response.json().await {
        Ok(response) => response,
        Err(err) => return Err(err.without_url().into()),
    };

    tracing::debug!(game_id, "exchanged gog cloud-saves session");
    Ok(GameToken {
        access_token: response.access_token,
        user_id: response.user_id,
        client_id,
    })
}

/// Lists every file currently in this game's cloud-saves bucket, with
/// tombstoned (deleted) entries already filtered out.
async fn list_files(http: &reqwest::Client, token: &GameToken) -> Result<Vec<CloudFile>> {
    #[derive(serde::Deserialize)]
    struct Entry {
        name: String,
        hash: String,
        last_modified: String,
    }

    let url = format!("{CLOUDSTORAGE}/v1/{}/{}", token.user_id, token.client_id);
    let response = match http
        .get(&url)
        .bearer_auth(&token.access_token)
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(error = %err, "gog cloud saves listing request failed, aborting sync");
            return Err(err.into());
        }
    };

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        tracing::debug!(file_count = 0, "listed gog cloud saves (bucket doesn't exist yet)");
        return Ok(Vec::new());
    }

    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(error = %err, "gog cloud saves listing request rejected, aborting sync");
            return Err(anyhow::Error::new(err).context("GOG cloud saves listing request rejected"));
        }
    };
    let entries: Vec<Entry> = match response.json().await {
        Ok(entries) => entries,
        Err(err) => return Err(err.into()),
    };

    let files: Result<Vec<CloudFile>> = entries
        .into_iter()
        .filter(|entry| entry.hash != TOMBSTONE_MD5)
        .map(|entry| {
            let last_modified = match DateTime::parse_from_rfc3339(&entry.last_modified) {
                Ok(last_modified) => last_modified.with_timezone(&Utc),
                Err(err) => {
                    return Err(
                        anyhow::Error::new(err).context(format!("bad last_modified timestamp on {}", entry.name))
                    );
                }
            };
            Ok(CloudFile {
                relative_path: entry.name,
                md5: entry.hash,
                last_modified,
            })
        })
        .collect();
    if let Ok(files) = &files {
        tracing::debug!(file_count = files.len(), "listed gog cloud saves");
    }
    files
}

/// Pushes one local file to the cloud, gzip-compressed with a fixed mtime
/// so re-uploading identical content produces an identical Etag.
async fn upload_file(http: &reqwest::Client, token: &GameToken, local_root: &Path, relative_path: &str) -> Result<()> {
    let absolute = local_root.join(relative_path);
    let bytes = match tokio::fs::read(&absolute).await {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(path = %absolute.display(), error = %err, "reading save file to upload failed, aborting sync");
            return Err(anyhow::Error::new(err).context(format!("reading {}", absolute.display())));
        }
    };
    let metadata = match tokio::fs::metadata(&absolute).await {
        Ok(metadata) => metadata,
        Err(err) => return Err(anyhow::Error::new(err).context(format!("reading metadata for {}", absolute.display()))),
    };
    let modified: DateTime<Utc> = match metadata.modified() {
        Ok(modified) => modified.into(),
        Err(err) => return Err(err.into()),
    };

    let compressed = match gzip_compress(&bytes) {
        Ok(compressed) => compressed,
        Err(err) => return Err(err),
    };
    let etag = format!("{:x}", md5::compute(&compressed));
    let url = file_url(token, relative_path);

    let request = http
        .put(&url)
        .bearer_auth(&token.access_token)
        .header("Content-Encoding", "gzip")
        .header("Etag", etag)
        .header(
            "X-Object-Meta-LocalLastModified",
            modified.to_rfc3339_opts(SecondsFormat::Secs, false),
        )
        .body(compressed);
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(relative_path, error = %err, "uploading cloud save failed, aborting sync");
            return Err(err.into());
        }
    };
    if let Err(err) = response.error_for_status() {
        tracing::error!(relative_path, error = %err, "gog rejected cloud save upload, aborting sync");
        return Err(anyhow::Error::new(err).context(format!("uploading {relative_path}")));
    }

    tracing::debug!(relative_path, bytes = bytes.len(), "cloud save uploaded");
    Ok(())
}

/// Pulls one cloud file to disk, decompressing it and restoring the mtime
/// the uploader recorded (when the response carried one).
async fn download_file(http: &reqwest::Client, token: &GameToken, local_root: &Path, file: &CloudFile) -> Result<()> {
    // `relative_path` comes straight off the cloud listing, i.e. from the
    // network — a crafted or corrupted entry with `..` or an absolute path
    // must not be able to write outside `local_root`.
    let Some(sanitized) = decanter_runtime::sanitize_relative_path(&file.relative_path) else {
        anyhow::bail!("cloud save entry has an unusable path: {}", file.relative_path);
    };
    let url = file_url(token, &file.relative_path);
    let response = match http.get(&url).bearer_auth(&token.access_token).send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(relative_path = file.relative_path, error = %err, "downloading cloud save failed, aborting sync");
            return Err(err.into());
        }
    };
    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(relative_path = file.relative_path, error = %err, "gog rejected cloud save download, aborting sync");
            return Err(anyhow::Error::new(err).context(format!("downloading {}", file.relative_path)));
        }
    };

    let local_last_modified = response
        .headers()
        .get("X-Object-Meta-LocalLastModified")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|parsed| parsed.with_timezone(&Utc));

    let compressed = match response.bytes().await {
        Ok(compressed) => compressed,
        Err(err) => return Err(err.into()),
    };
    let decompressed = match gzip_decompress(&compressed) {
        Ok(decompressed) => decompressed,
        Err(err) => return Err(err),
    };

    let absolute = local_root.join(sanitized);
    if let Some(parent) = absolute.parent() {
        if let Err(err) = tokio::fs::create_dir_all(parent).await {
            return Err(err.into());
        }
    }
    if let Err(err) = tokio::fs::write(&absolute, &decompressed).await {
        return Err(anyhow::Error::new(err).context(format!("writing {}", absolute.display())));
    }

    if let Some(modified) = local_last_modified {
        if let Err(err) = set_mtime(&absolute, modified) {
            return Err(err);
        }
    }

    tracing::debug!(
        relative_path = file.relative_path,
        bytes = decompressed.len(),
        "cloud save downloaded"
    );
    Ok(())
}

/// Removes one file from the cloud bucket outright (used to mirror a local
/// deletion when uploading).
async fn delete_file(http: &reqwest::Client, token: &GameToken, relative_path: &str) -> Result<()> {
    let url = file_url(token, relative_path);
    let response = match http.delete(&url).bearer_auth(&token.access_token).send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(relative_path, error = %err, "deleting cloud save failed, aborting sync");
            return Err(err.into());
        }
    };
    if let Err(err) = response.error_for_status() {
        tracing::error!(relative_path, error = %err, "gog rejected cloud save deletion, aborting sync");
        return Err(anyhow::Error::new(err).context(format!("deleting {relative_path} from GOG cloud saves")));
    }
    tracing::debug!(relative_path, "cloud save deleted");
    Ok(())
}

/// Closes out the sync transaction. Best-effort by design at the call
/// site — [`sync`] still reports what actually moved even if this fails,
/// since the files themselves are already where they should be.
async fn commit(http: &reqwest::Client, token: &GameToken) -> Result<()> {
    let url = format!("{CLOUDSTORAGE}/v1/{}/{}", token.user_id, token.client_id);
    let request = http.post(&url).bearer_auth(&token.access_token);
    let response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(error = %err, "gog cloud saves commit request failed");
            return Err(err.into());
        }
    };
    if let Err(err) = response.error_for_status() {
        tracing::error!(error = %err, "gog cloud saves commit rejected");
        return Err(anyhow::Error::new(err).context("GOG cloud saves commit rejected"));
    }
    tracing::debug!("gog cloud saves commit completed");
    Ok(())
}

/// Builds the per-file URL a given relative path lives at in this game's bucket.
fn file_url(token: &GameToken, relative_path: &str) -> String {
    format!(
        "{CLOUDSTORAGE}/v1/{}/{}/{}",
        token.user_id,
        token.client_id,
        encode_relative_path(relative_path)
    )
}

/// Percent-encodes each path segment, keeping `/` as a path separator —
/// same shape as Python's `urllib.parse.quote(path)`, whose default `safe`
/// set is `/`.
fn encode_relative_path(path: &str) -> String {
    path.split('/').map(encode_segment).collect::<Vec<_>>().join("/")
}

/// Percent-encodes a single path segment (no `/` handling — see [`encode_relative_path`]).
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(char::from(*byte)),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Gzips `data` at a fixed mtime, matching gogdl's `gzip.compress(data, 6, mtime=0)`.
fn gzip_compress(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::new(6));
    if let Err(err) = encoder.write_all(data) {
        return Err(err.into());
    }
    match encoder.finish() {
        Ok(compressed) => Ok(compressed),
        Err(err) => Err(err.into()),
    }
}

/// Inflates gzip content downloaded from the cloud bucket.
fn gzip_decompress(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut decoder = flate2::read::GzDecoder::new(data);
    let mut out = Vec::new();
    if let Err(err) = decoder.read_to_end(&mut out) {
        return Err(err.into());
    }
    Ok(out)
}

/// Restores a file's mtime to the value a previous uploader recorded.
fn set_mtime(path: &Path, modified: DateTime<Utc>) -> Result<()> {
    let ts = modified.timestamp();
    let system_time = if ts >= 0 {
        let secs = u64::try_from(ts).unwrap_or(0);
        SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_secs(secs))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    } else {
        let secs = u64::try_from(-ts).unwrap_or(0);
        SystemTime::UNIX_EPOCH
            .checked_sub(Duration::from_secs(secs))
            .unwrap_or(SystemTime::UNIX_EPOCH)
    };
    let file = match File::options().write(true).open(path) {
        Ok(file) => file,
        Err(err) => {
            return Err(anyhow::Error::new(err).context(format!("opening {} to restore its timestamp", path.display())));
        }
    };
    if let Err(err) = file.set_modified(system_time) {
        return Err(anyhow::Error::new(err).context(format!("setting mtime on {}", path.display())));
    }
    Ok(())
}

/// Walks `root` recursively, collecting every regular file under it.
fn scan_local_files(root: &Path) -> Result<Vec<LocalFile>> {
    let mut out = Vec::new();
    if let Err(err) = scan_dir(root, root, &mut out) {
        tracing::error!(root = %root.display(), error = %err, "scanning local save files failed, aborting sync");
        return Err(err);
    }
    tracing::debug!(root = %root.display(), file_count = out.len(), "scanned local save files");
    Ok(out)
}

/// Recursive step for [`scan_local_files`]; `root` stays fixed across the
/// recursion so every entry's `relative_path` is relative to it, not `dir`.
fn scan_dir(root: &Path, dir: &Path, out: &mut Vec<LocalFile>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => return Err(anyhow::Error::new(err).context(format!("reading {}", dir.display()))),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => return Err(err.into()),
        };
        let path = entry.path();
        if path.is_dir() {
            if let Err(err) = scan_dir(root, &path, out) {
                return Err(err);
            }
            continue;
        }
        let relative_path = match path.strip_prefix(root) {
            Ok(relative) => relative,
            Err(err) => {
                return Err(anyhow::Error::new(err).context(format!(
                    "{} is not under {}",
                    path.display(),
                    root.display()
                )));
            }
        }
        .to_string_lossy()
        .replace('\\', "/");
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(err) => return Err(err.into()),
        };
        let modified: DateTime<Utc> = match metadata.modified() {
            Ok(modified) => modified.into(),
            Err(err) => return Err(err.into()),
        };
        out.push(LocalFile {
            relative_path,
            modified,
        });
    }
    Ok(())
}
