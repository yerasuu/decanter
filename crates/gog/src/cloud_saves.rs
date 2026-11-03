// SPDX-License-Identifier: GPL-3.0-or-later
//! Syncing a save folder the user chose against the game's GOG cloud storage.
//!
//! GOG documents none of it; `docs/gog-cloud-saves-behaviour-spec.md` records what the service
//! does and states the design followed here (its section 4): the only memory is the `last_sync`
//! instant decanter already keeps per game, every path is decided before anything moves, and a
//! conflict on any path moves nothing at all.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use core::time::Duration;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use decanter_core::platform;
use decanter_core::secrets::Secret;
use flate2::read::GzDecoder;
use flate2::{Compression, GzBuilder};
use tokio::fs as async_fs;
use tokio::task::spawn_blocking;

use crate::api::client::{Bucket, CloudObject, Error, Sessionless, Token};
use crate::{depot, session};

pub type Result<T> = core::result::Result<T, anyhow::Error>;

/// What the listing holds for a file Galaxy deleted. Compared as given and never computed, since
/// how GOG produces it is unknown (spec 2.5).
const TOMBSTONE: &str = "aadd86936a80ee8a369579c3926f1b3c";

/// The location bucket names take when a game declares none.
const DEFAULT_LOCATION: &str = "__default";

/// Marks a download being written; the scan skips such files, and cloud names ending in it are
/// refused so that a synced file can never be mistaken for one.
const PARTIAL_SUFFIX: &str = ".decanter-partial";

/// The download's own cap, for every save body held in memory: read, gzipped or inflated.
const MAX_SAVE_FILE: usize = 256 << 20;

/// Level 6 with a zero header time makes the gzip of unchanged content byte-identical on every
/// upload, so that its md5 matches what the listing reports for it (spec 2.3).
const GZIP_LEVEL: u32 = 6;

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
    /// The two sides disagree on a path in a way `last_sync` cannot settle; nothing was moved.
    Conflict,
    None,
}

/// A direction the user picked to resolve a conflict. Under it nothing is deleted on either side.
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
    /// Unix seconds of the sync's start, to be recorded as the game's new `last_sync`; meaningless
    /// on a conflict, which the caller must not record.
    pub last_sync: i64,
}

/// A session scoped to one game build's Galaxy client: the only one its cloud storage and stats
/// answer to.
pub(crate) struct GameToken {
    pub access_token: Secret,
    /// From the exchange's answer: the bucket's path segment.
    pub user_id: String,
    pub client_id: String,
}

/// A game build's own Galaxy client credentials, from its build manifest.
pub(crate) struct BuildClient {
    pub client_id: String,
    pub client_secret: Secret,
}

/// One game's bucket and the client that reaches it, with the product id every log line names.
struct Store<'sync> {
    http: &'sync Sessionless,
    bucket: &'sync Bucket,
    product_id: &'sync str,
}

struct LocalFile {
    path: PathBuf,
    /// Unix seconds, truncated.
    modified: i64,
}

struct Remote {
    name: String,
    hash: Option<String>,
    /// `None` when the listing's time did not parse, which reads as changed.
    modified: Option<DateTime<Utc>>,
}

/// The cloud side as the decision reads it, by path below the location name.
struct Cloud {
    files: BTreeMap<String, Remote>,
    /// Paths whose only entry is a tombstone, with that entry's name, so that an upload of them
    /// goes back under the location they came from.
    tombstoned: BTreeMap<String, String>,
    locations: BTreeSet<String>,
    /// GOG answered 404, which it was not seen to do (an empty bucket answers 200 with no
    /// entries): the absence of a file there then says nothing about a deletion.
    is_missing: bool,
}

enum Step<'cloud> {
    Upload {
        relative: String,
        name: String,
    },
    Download {
        relative: String,
        remote: &'cloud Remote,
    },
    DeleteLocal {
        relative: String,
    },
    DeleteCloud {
        relative: String,
        name: &'cloud str,
    },
    /// Equal content whose local time is set to the cloud's, so that it no longer reads as changed.
    Touch {
        relative: String,
        modified: SystemTime,
    },
}

enum Plan<'cloud> {
    Steps(Vec<Step<'cloud>>),
    Conflict { relative: String },
}

/// Syncs `local_root` with the cloud storage of `product_id`'s build for this platform.
///
/// With `force`, every file that differs is sent that way and nothing is deleted. Without it each
/// path goes the way that changed since `last_sync`, a path only one side still has is deleted on
/// the other when it did not change since `last_sync` (never without a `last_sync`), and a path
/// both sides changed, or that differs with neither having changed, makes the whole sync a
/// [`SyncAction::Conflict`] that moves nothing. The first failure aborts the sync; what already
/// moved stays moved.
///
/// # Errors
///
/// A product id that is not a positive number, the save folder unusable, no stored session or GOG
/// refusing it, GOG unreachable or answering something unreadable at any step, and a save file
/// that cannot be read or written.
#[inline]
pub async fn sync(
    account_id: &str,
    product_id: &str,
    local_root: &Path,
    last_sync: Option<i64>,
    force: Option<ForceDirection>,
) -> Result<SyncOutcome> {
    let started = Utc::now().timestamp();
    let game_id = product_id
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .context("GOG product_id must be a positive number")?;
    async_fs::create_dir_all(local_root)
        .await
        .with_context(|| format!("creating the save folder {}", local_root.display()))?;

    let game = game_session(account_id, product_id, game_id).await?;
    let http = Sessionless::new().context("building the GOG HTTP client")?;
    let bucket = Bucket::new(&game.user_id, &game.client_id, game.access_token)
        .map_err(|err| session::failed("GOG cloud saves session is unusable", err))?;

    let local = scan(local_root).await.inspect_err(|err| {
        tracing::error!(product_id, error = %format_args!("{err:#}"), "scanning local save files failed, aborting sync");
    })?;
    let store = Store {
        http: &http,
        bucket: &bucket,
        product_id,
    };
    let cloud = list(&store).await?;

    let plan = match force {
        Some(direction) => forced(direction, &local, &cloud).await?,
        None => decided(&store, &local, &cloud, last_sync, started).await?,
    };
    let steps = match plan {
        Plan::Steps(steps) => steps,
        Plan::Conflict { relative } => {
            tracing::info!(product_id, relative, "cloud saves conflict, nothing moved");
            return Ok(SyncOutcome {
                action: SyncAction::Conflict,
                uploaded: 0,
                downloaded: 0,
                last_sync: started,
            });
        }
    };

    let moved = run(&store, local_root, steps).await?;
    if moved.is_cloud_changed {
        commit(&store).await;
    }
    let action = match force {
        Some(ForceDirection::Upload) => SyncAction::Upload,
        Some(ForceDirection::Download) => SyncAction::Download,
        None if moved.is_cloud_changed => SyncAction::Upload,
        None if moved.is_local_changed => SyncAction::Download,
        None => SyncAction::None,
    };
    tracing::info!(
        product_id,
        action = ?action,
        uploaded = moved.uploaded,
        downloaded = moved.downloaded,
        "cloud saves synced"
    );
    Ok(SyncOutcome {
        action,
        uploaded: moved.uploaded,
        downloaded: moved.downloaded,
        last_sync: started,
    })
}

/// The game session for `account_id`'s stored session, held to that same account.
async fn game_session(account_id: &str, product_id: &str, game_id: i64) -> Result<GameToken> {
    let token_failed = |err: &anyhow::Error| {
        let error = format!("{err:#}");
        if err
            .chain()
            .any(|cause| matches!(cause.downcast_ref(), Some(Error::Rejected { .. })))
        {
            tracing::error!(product_id, error, "gog refused a cloud-saves session, aborting sync");
        } else {
            tracing::error!(product_id, error, "gog cloud-saves token request failed, aborting sync");
        }
    };
    let live = session::live_token(account_id).await.inspect_err(token_failed)?;
    let game = match exchange_game_token(&live, game_id).await {
        Ok(game) => game,
        Err(err) => {
            token_failed(&err);
            return Err(anyhow::anyhow!("GOG cloud saves session failed: {err:#}"));
        }
    };
    // Otherwise the bucket of another account would be synced into this one's save folder.
    if game.user_id != live.user_id {
        let err = anyhow::anyhow!("GOG exchanged a cloud saves session for another account");
        token_failed(&err);
        return Err(err);
    }
    Ok(game)
}

/// The Galaxy client of the build decanter selects for `game_id` on this platform.
///
/// # Errors
///
/// GOG listing no build, a manifest naming no client or secret, and GOG's own errors, kept typed
/// in the chain.
pub(crate) async fn resolve_build_client(access_token: &str, game_id: i64) -> Result<BuildClient> {
    let http = Sessionless::new().context("building the GOG HTTP client")?;
    let builds = depot::get_builds(&http, access_token, game_id, platform::game_system())
        .await
        .inspect_err(|err| {
            tracing::error!(game_id, error = %format_args!("{err:#}"), "gog builds lookup failed, aborting cloud saves client resolution");
        })?;
    let build = depot::select_build(&builds).context("GOG listed no build to take a cloud saves client from")?;
    let meta = http.build_meta(access_token, &build.link).await.inspect_err(|err| {
        tracing::error!(game_id, error = %err, "gog build manifest fetch failed, aborting cloud saves client resolution");
    })?;
    let client_id = meta
        .client_id
        .filter(|id| !id.is_empty())
        .context("the build manifest names no Galaxy client")?;
    let client_secret = meta
        .client_secret
        .filter(|secret| !secret.is_empty())
        .map(Secret::from)
        .context("the build manifest has no Galaxy client secret")?;
    Ok(BuildClient {
        client_id,
        client_secret,
    })
}

/// Trades `session` for one scoped to `game_id`'s build client. The account's refresh token is
/// spent without opening a new login session, and nothing of the answer but the game's access
/// token is kept, so `session` stays the one to store.
///
/// # Errors
///
/// As [`resolve_build_client`], and GOG refusing or failing the exchange; every
/// [`crate::api::client::Error`] stays typed in the chain, where callers tell a refusal from GOG
/// being unreachable.
pub(crate) async fn exchange_game_token(session: &Token, game_id: i64) -> Result<GameToken> {
    let build = resolve_build_client(session.access_token.expose(), game_id).await?;
    let http = Sessionless::new()?;
    let game = http
        .game_session(&build.client_id, &build.client_secret, &session.refresh_token)
        .await?;
    Ok(GameToken {
        access_token: game.access_token,
        user_id: game.user_id,
        client_id: build.client_id,
    })
}

/// GOG answered and turned the request down, as opposed to not answering or answering garbage.
const fn is_refusal(err: &Error) -> bool {
    matches!(err, Error::Rejected { .. } | Error::Unavailable { .. })
}

async fn list(store: &Store<'_>) -> Result<Cloud> {
    let &Store {
        http,
        bucket,
        product_id,
    } = store;
    let listing = http.cloud_objects(bucket).await.map_err(|err| {
        if is_refusal(&err) {
            tracing::error!(product_id, error = %err, "gog cloud saves listing request rejected, aborting sync");
        } else {
            tracing::error!(product_id, error = %err, "gog cloud saves listing request failed, aborting sync");
        }
        session::failed("GOG cloud saves listing failed", err)
    })?;
    let mut cloud = Cloud {
        files: BTreeMap::new(),
        tombstoned: BTreeMap::new(),
        locations: BTreeSet::new(),
        is_missing: listing.is_none(),
    };
    let mut refused = 0_usize;
    for CloudObject {
        name,
        hash,
        last_modified,
        ..
    } in listing.into_iter().flatten()
    {
        let Some((location, relative)) =
            split_name(&name).map(|(location, path)| (location.map(str::to_owned), path.to_owned()))
        else {
            refused = refused.saturating_add(1);
            tracing::warn!(product_id, name, "cloud save name is unsafe as a local path, skipped");
            continue;
        };
        cloud.locations.extend(location);
        if hash.as_deref() == Some(TOMBSTONE) {
            cloud.tombstoned.entry(relative).or_insert(name);
            continue;
        }
        if cloud.files.contains_key(&relative) {
            // The folder is synced flat by the path below the location, so two entries, under two
            // locations or one flat and one under a location, would land on one local file.
            tracing::error!(
                product_id,
                relative,
                "gog cloud saves listing request rejected, aborting sync"
            );
            anyhow::bail!("GOG cloud saves listing has {relative} more than once");
        }
        cloud.files.insert(
            relative,
            Remote {
                name,
                hash,
                modified: last_modified,
            },
        );
    }
    if refused > 0 {
        tracing::warn!(product_id, refused, "cloud saves listing has unsafe names, skipped");
    }
    tracing::debug!(
        product_id,
        files = cloud.files.len(),
        tombstones = cloud.tombstoned.len(),
        is_missing = cloud.is_missing,
        "listed cloud saves"
    );
    Ok(cloud)
}

/// A bucket name as `(location, path below it)`, both safe to become a local path, or `None`. A
/// name with no `/` has no location and is its own path: earlier decanter versions synced flat
/// paths, and live buckets hold such entries beside Galaxy's location-qualified ones (spec 6.1).
fn split_name(name: &str) -> Option<(Option<&str>, &str)> {
    let (location, relative) = match name.split_once('/') {
        Some((location, relative)) => (Some(location), relative),
        None => (None, name),
    };
    (location.is_none_or(is_safe_relative) && is_safe_relative(relative) && !relative.ends_with(PARTIAL_SUFFIX))
        .then_some((location, relative))
}

/// A `/`-separated path that stays below the folder it is joined to on every platform: no empty,
/// `.` or `..` segment, nothing absolute, no `\`, NUL or drive prefix in any segment, which Windows
/// would otherwise read as a separator or as a new root.
fn is_safe_relative(path: &str) -> bool {
    let is_drive = |segment: &str| matches!(segment.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    !path.contains(['\\', '\0'])
        && path
            .split('/')
            .all(|segment| !matches!(segment, "" | "." | "..") && !is_drive(segment))
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn local_path(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    path.extend(relative.split('/'));
    path
}

async fn scan(root: &Path) -> Result<BTreeMap<String, LocalFile>> {
    let owned = root.to_path_buf();
    let (files, skipped) = spawn_blocking(move || scan_blocking(&owned))
        .await
        .context("save scan worker task panicked")?
        .context("scanning the save folder")?;
    if skipped > 0 {
        tracing::warn!(skipped, "save files whose names cannot be cloud names, skipped");
    }
    Ok(files)
}

/// Every regular file below `root` by its `/`-separated path, symlinks and partial downloads left
/// out, and how many were skipped for a name that cannot be a cloud name.
fn scan_blocking(root: &Path) -> io::Result<(BTreeMap<String, LocalFile>, usize)> {
    let mut files = BTreeMap::new();
    let mut skipped = 0_usize;
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for listed in fs::read_dir(&dir)? {
            let entry = listed?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let Some(relative) = relative_name(root, &path) else {
                skipped = skipped.saturating_add(1);
                continue;
            };
            if relative.ends_with(PARTIAL_SUFFIX) {
                continue;
            }
            let modified = unix_seconds(entry.metadata()?.modified()?);
            files.insert(relative, LocalFile { path, modified });
        }
    }
    Ok((files, skipped))
}

fn relative_name(root: &Path, path: &Path) -> Option<String> {
    let segments = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|component| match component {
            Component::Normal(segment) => segment.to_str(),
            Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let relative = segments.join("/");
    is_safe_relative(&relative).then_some(relative)
}

/// Whole seconds, so that a time compares equal to the same time written with no fraction.
fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_secs()).map_or(i64::MIN, |secs| 0_i64.saturating_sub(secs)),
    }
}

fn system_time(unix: i64) -> SystemTime {
    let magnitude = Duration::from_secs(unix.unsigned_abs());
    if unix >= 0 {
        UNIX_EPOCH.checked_add(magnitude)
    } else {
        UNIX_EPOCH.checked_sub(magnitude)
    }
    .unwrap_or(UNIX_EPOCH)
}

/// Every path under a forced direction: whatever differs goes that way, and nothing is deleted.
async fn forced<'cloud>(
    direction: ForceDirection,
    local: &BTreeMap<String, LocalFile>,
    cloud: &'cloud Cloud,
) -> Result<Plan<'cloud>> {
    let mut steps = Vec::new();
    match direction {
        ForceDirection::Upload => {
            for (relative, file) in local {
                let listed = cloud.files.get(relative);
                if let Some(remote) = listed
                    && is_same_by_hash(file, remote).await?
                {
                    continue;
                }
                steps.push(Step::Upload {
                    relative: relative.clone(),
                    name: upload_name(cloud, relative, listed)?,
                });
            }
        }
        ForceDirection::Download => {
            for (relative, remote) in &cloud.files {
                if let Some(file) = local.get(relative)
                    && is_same_by_hash(file, remote).await?
                {
                    continue;
                }
                steps.push(Step::Download {
                    relative: relative.clone(),
                    remote,
                });
            }
        }
    }
    Ok(Plan::Steps(steps))
}

/// Every path decided against `last_sync` (spec 4.3), stopping at the first conflict.
async fn decided<'cloud>(
    store: &Store<'_>,
    local: &BTreeMap<String, LocalFile>,
    cloud: &'cloud Cloud,
    last_sync: Option<i64>,
    started: i64,
) -> Result<Plan<'cloud>> {
    let is_local_changed = |file: &LocalFile| last_sync.is_none_or(|synced| file.modified > synced);
    let is_cloud_changed = |remote: &Remote| match (last_sync, remote.modified) {
        (Some(synced), Some(modified)) => modified.timestamp() > synced,
        (None, _) | (_, None) => true,
    };
    let paths: BTreeSet<&String> = local.keys().chain(cloud.files.keys()).collect();
    let mut steps = Vec::new();
    for relative in paths {
        let step = match (local.get(relative), cloud.files.get(relative)) {
            (Some(file), None) if is_local_changed(file) => Step::Upload {
                relative: relative.clone(),
                name: upload_name(cloud, relative, None)?,
            },
            (Some(_), None) if cloud.is_missing => {
                tracing::warn!(
                    product_id = store.product_id,
                    relative,
                    "cloud saves bucket answered 404, local save the cloud lacks kept"
                );
                continue;
            }
            (Some(_), None) => Step::DeleteLocal {
                relative: relative.clone(),
            },
            (None, Some(remote)) if is_cloud_changed(remote) => Step::Download {
                relative: relative.clone(),
                remote,
            },
            (None, Some(remote)) => Step::DeleteCloud {
                relative: relative.clone(),
                name: &remote.name,
            },
            (Some(file), Some(remote)) => {
                if is_same_by_hash(file, remote).await? {
                    continue;
                }
                match (is_local_changed(file), is_cloud_changed(remote)) {
                    (true, false) => Step::Upload {
                        relative: relative.clone(),
                        name: remote.name.clone(),
                    },
                    (false, true) => Step::Download {
                        relative: relative.clone(),
                        remote,
                    },
                    (true, true) | (false, false) => {
                        let Some(modified) = equal_content(store, file, remote).await? else {
                            return Ok(Plan::Conflict {
                                relative: relative.clone(),
                            });
                        };
                        // With no time from the cloud, the sync's own start keeps the file from
                        // reading as changed next time.
                        Step::Touch {
                            relative: relative.clone(),
                            modified: system_time(modified.map_or(started, |time| time.timestamp())),
                        }
                    }
                }
            }
            (None, None) => continue,
        };
        steps.push(step);
    }
    Ok(Plan::Steps(steps))
}

/// The bucket name a local file goes up as: the name its cloud entry or tombstone already has,
/// flat ones included, else the path under the one location the bucket's qualified names use,
/// `__default` when they use none.
fn upload_name(cloud: &Cloud, relative: &str, remote: Option<&Remote>) -> Result<String> {
    if let Some(name) = remote
        .map(|listed| &listed.name)
        .or_else(|| cloud.tombstoned.get(relative))
    {
        return Ok(name.clone());
    }
    let mut locations = cloud.locations.iter();
    match (locations.next(), locations.next()) {
        (None, _) => Ok(format!("{DEFAULT_LOCATION}/{relative}")),
        (Some(location), None) => Ok(format!("{location}/{relative}")),
        (Some(_), Some(_)) => anyhow::bail!(
            "the cloud saves bucket uses several locations, so the one for the new save {relative} is unknown"
        ),
    }
}

/// Only true for an object decanter's own encoder wrote; a `false` says nothing about the content
/// (spec 2.3).
async fn is_same_by_hash(file: &LocalFile, remote: &Remote) -> Result<bool> {
    let Some(hash) = remote.hash.as_deref() else {
        return Ok(false);
    };
    let path = file.path.clone();
    let gzipped = blocking(move || {
        let (body, _) = read_local(&path)?;
        gzip(&body)
    })
        .await
        .inspect_err(|err| {
            tracing::error!(path = %file.path.display(), error = %format_args!("{err:#}"), "reading save file to upload failed, aborting sync");
        })
        .with_context(|| format!("reading the save file {}", file.path.display()))?;
    Ok(format!("{:x}", md5::compute(&gzipped)) == hash)
}

/// Downloads `remote` to compare it with `file` byte for byte: `Some` with the cloud's recorded
/// time (`None` when it has none) when they are equal, `None` when they differ.
async fn equal_content(store: &Store<'_>, file: &LocalFile, remote: &Remote) -> Result<Option<Option<DateTime<Utc>>>> {
    let (body, modified) = download(store, remote).await?;
    let path = file.path.clone();
    let (local, _) = blocking(move || read_local(&path))
        .await
        .inspect_err(|err| {
            tracing::error!(path = %file.path.display(), error = %format_args!("{err:#}"), "reading save file to upload failed, aborting sync");
        })
        .with_context(|| format!("reading the save file {}", file.path.display()))?;
    Ok((local == body).then_some(modified))
}

/// The body of `remote` with its gzip undone, and the file's own time: the uploader's header, else
/// the listing's.
async fn download(store: &Store<'_>, remote: &Remote) -> Result<(Vec<u8>, Option<DateTime<Utc>>)> {
    let &Store {
        http,
        bucket,
        product_id,
    } = store;
    let name = remote.name.as_str();
    let downloaded = http.cloud_download(bucket, name).await.map_err(|err| {
        if is_refusal(&err) {
            tracing::error!(product_id, name, error = %err, "gog rejected cloud save download, aborting sync");
        } else {
            tracing::error!(product_id, name, error = %err, "downloading cloud save failed, aborting sync");
        }
        session::failed(&format!("GOG cloud save download of {name} failed"), err)
    })?;
    let modified = downloaded.local_last_modified.or(remote.modified);
    let body = decoded(downloaded.body).map_err(|err| {
        tracing::error!(product_id, name, error = %err, "downloading cloud save failed, aborting sync");
        anyhow::anyhow!("GOG cloud save {name} is unreadable: {err}")
    })?;
    Ok((body, modified))
}

/// The stored object is gzip, and whether the HTTP layer already undid it cannot be assumed, so the
/// body's own first bytes decide.
fn decoded(body: Vec<u8>) -> io::Result<Vec<u8>> {
    if !body.starts_with(&[0x1f, 0x8b]) {
        return Ok(body);
    }
    let mut plain = Vec::new();
    let limit = u64::try_from(MAX_SAVE_FILE).unwrap_or(u64::MAX).saturating_add(1);
    GzDecoder::new(body.as_slice()).take(limit).read_to_end(&mut plain)?;
    if plain.len() > MAX_SAVE_FILE {
        return Err(io::Error::other(format!("inflates past {MAX_SAVE_FILE} bytes")));
    }
    Ok(plain)
}

/// The file's content and modification time, refused past [`MAX_SAVE_FILE`] bytes.
fn read_local(path: &Path) -> io::Result<(Vec<u8>, SystemTime)> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let length = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
    if length > MAX_SAVE_FILE {
        return Err(io::Error::other(format!("larger than {MAX_SAVE_FILE} bytes")));
    }
    let mut body = Vec::with_capacity(length);
    file.read_to_end(&mut body)?;
    Ok((body, metadata.modified()?))
}

fn gzip(body: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::new(GZIP_LEVEL));
    encoder.write_all(body)?;
    encoder.finish()
}

#[derive(Default)]
struct Moved {
    uploaded: usize,
    downloaded: usize,
    is_cloud_changed: bool,
    is_local_changed: bool,
}

/// Carries out `steps` in order, stopping at the first that fails.
async fn run(store: &Store<'_>, root: &Path, steps: Vec<Step<'_>>) -> Result<Moved> {
    let &Store {
        http,
        bucket,
        product_id,
    } = store;
    let mut moved = Moved::default();
    for step in steps {
        match step {
            Step::Upload { relative, name } => {
                upload(store, &local_path(root, &relative), &name).await?;
                moved.uploaded = moved.uploaded.saturating_add(1);
                moved.is_cloud_changed = true;
            }
            Step::Download { relative, remote } => {
                let (body, recorded) = download(store, remote).await?;
                let modified = recorded.map_or_else(SystemTime::now, |time| system_time(time.timestamp()));
                let path = local_path(root, &relative);
                blocking(move || write_atomically(&path, &body, modified))
                    .await
                    .inspect_err(|err| {
                        tracing::error!(product_id, relative, error = %format_args!("{err:#}"), "downloading cloud save failed, aborting sync");
                    })
                    .with_context(|| format!("writing the downloaded save {relative}"))?;
                moved.downloaded = moved.downloaded.saturating_add(1);
                moved.is_local_changed = true;
            }
            Step::DeleteLocal { relative } => {
                let path = local_path(root, &relative);
                blocking(move || match fs::remove_file(&path) {
                    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                    removed => removed,
                })
                .await
                .inspect_err(|err| {
                    tracing::error!(product_id, relative, error = %format_args!("{err:#}"), "couldn't remove local save file the cloud no longer has");
                })
                .with_context(|| format!("removing the local save {relative} the cloud no longer has"))?;
                moved.is_local_changed = true;
            }
            Step::DeleteCloud { relative, name } => {
                http.cloud_delete(bucket, name).await.map_err(|err| {
                    if is_refusal(&err) {
                        tracing::error!(product_id, name, error = %err, "gog rejected cloud save deletion, aborting sync");
                    } else {
                        tracing::error!(product_id, name, error = %err, "deleting cloud save failed, aborting sync");
                    }
                    session::failed(&format!("GOG cloud save deletion of {relative} failed"), err)
                })?;
                moved.is_cloud_changed = true;
            }
            Step::Touch { relative, modified } => {
                let path = local_path(root, &relative);
                blocking(move || {
                    let file = OpenOptions::new().write(true).open(&path)?;
                    file.set_modified(modified)?;
                    file.sync_all()
                })
                .await
                .inspect_err(|err| {
                    tracing::error!(product_id, relative, error = %format_args!("{err:#}"), "downloading cloud save failed, aborting sync");
                })
                .with_context(|| format!("setting the time of the save {relative}"))?;
            }
        }
    }
    Ok(moved)
}

async fn upload(store: &Store<'_>, path: &Path, name: &str) -> Result<()> {
    let &Store {
        http,
        bucket,
        product_id,
    } = store;
    let owned = path.to_path_buf();
    let (gzipped, mtime) = blocking(move || {
        let (body, read_at) = read_local(&owned)?;
        Ok((gzip(&body)?, read_at))
    })
    .await
    .inspect_err(|err| {
        tracing::error!(product_id, name, error = %format_args!("{err:#}"), "reading save file to upload failed, aborting sync");
    })
    .with_context(|| format!("reading the save file {}", path.display()))?;
    let modified = DateTime::from_timestamp(unix_seconds(mtime), 0)
        .with_context(|| format!("the save file {} has a time out of range", path.display()))?;
    http.cloud_upload(bucket, name, gzipped, modified).await.map_err(|err| {
        if is_refusal(&err) {
            tracing::error!(product_id, name, error = %err, "gog rejected cloud save upload, aborting sync");
        } else {
            tracing::error!(product_id, name, error = %err, "uploading cloud save failed, aborting sync");
        }
        session::failed(&format!("GOG cloud save upload of {name} failed"), err)
    })
}

/// A failed commit undoes nothing already stored, so it only warns.
async fn commit(store: &Store<'_>) {
    let &Store {
        http,
        bucket,
        product_id,
    } = store;
    match http.cloud_commit(bucket).await {
        Ok(()) => tracing::debug!(product_id, "committed cloud saves"),
        Err(err) if is_refusal(&err) => {
            tracing::warn!(product_id, error = %err, "gog cloud saves commit rejected");
        }
        Err(err) => tracing::warn!(product_id, error = %err, "gog cloud saves commit request failed"),
    }
}

async fn blocking<T>(work: impl FnOnce() -> io::Result<T> + Send + 'static) -> Result<T>
where
    T: Send + 'static,
{
    Ok(spawn_blocking(work).await.context("save file worker task panicked")??)
}

/// Replaces `target` with `body` through a fsynced sibling renamed over it, its time set before the
/// rename, so that the file never exists half written or with the wrong time; a failure leaves the
/// old file as it was.
fn write_atomically(target: &Path, body: &[u8], modified: SystemTime) -> io::Result<()> {
    let (Some(dir), Some(file_name)) = (target.parent(), target.file_name()) else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "save path has no folder"));
    };
    fs::create_dir_all(dir)?;
    let mut partial_name = file_name.to_os_string();
    partial_name.push(PARTIAL_SUFFIX);
    let partial = dir.join(partial_name);
    match fs::remove_file(&partial) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        Ok(()) | Err(_) => {}
    }
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .and_then(|mut file| {
            file.write_all(body)?;
            file.set_modified(modified)?;
            file.sync_all()
        });
    if let Err(err) = written.and_then(|()| fs::rename(&partial, target)) {
        drop(fs::remove_file(&partial));
        return Err(err);
    }
    // Windows opens no folder as a file to sync; NTFS journals the rename itself.
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    Ok(())
}
