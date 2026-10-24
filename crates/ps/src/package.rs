//! psport's Windows package as its CI publishes it (psport's `tools/win32-bundle.sh`): `psport.exe`, its
//! GPU library, its two helper programs and the DLLs they load, in one folder of a zip attached to each
//! `vX.Y.Z` release with its `.sha256` beside it. Only tagged releases are published; which ones carry
//! a package comes from the releases API of its repository, asked anonymously, and the package itself from the release's
//! downloads, as the Linux bundles ([`crate::bundle`]).
//!
//! A package is checked where it enters: its tag and asset names must be the exact published ones, the
//! archive must match the release's own checksum and size and stay under [`bundle::MAX_BYTES`], every
//! name in it is checked before `tar` extracts anything, into a staging folder, and what lands there
//! must be one folder of plain files whose programs are the four known ones and the DLLs its
//! `BUILD-INFO.txt` lists. Only then is it renamed into place and recorded, so a recorded folder at
//! [`dir`] is trusted as is. Nothing in it runs until the user plays.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::archive::{self, Limits, Names, Top};
use crate::bbport::direct::{EXECUTABLE, GPU_LIBRARY};
use crate::bundle;
use crate::paths::Root;
use crate::port::Port;
use crate::prebuilt;

/// The [`crate::bbport::LauncherSettings`] key the version Jugar runs is kept under.
pub const SELECTED_KEY: &str = "psport_windows_version";
/// Set when the user chose a version other than the newest, which updates then leave in place.
pub const PINNED_KEY: &str = "psport_windows_pinned";

/// What `tools/win32-bundle.sh` installs as programs; any other `.exe` is refused.
pub(crate) const PROGRAMS: [&str; 3] = [EXECUTABLE, "psport-gpu-capabilities.exe", "psport-windows-probe.exe"];
/// The testers' scripts from psport's `packaging/windows`, which decanter never runs.
const SCRIPTS: [&str; 2] = ["psport-probe.bat", "psport-probe.ps1"];
/// Every other file in the package is a text, by its extension.
const DOCUMENTS: [&str; 4] = ["txt", "md", "json", "ini"];
pub(crate) const BUILD_INFO: &str = "BUILD-INFO.txt";

/// Extracted, all files together, well above what four programs and a few dozen DLLs take; any one
/// file, where the largest ones, the GPU library and the codecs, are tens of megabytes.
const LIMITS: Limits = Limits {
    entries: 4096,
    unpacked_bytes: 1 << 30,
    file_bytes: 256 << 20,
};
/// Where a release download is redirected to: the release itself and its asset storage
/// ([self-hosted runner domains](https://docs.github.com/en/actions/reference/runners/self-hosted-runners)).
const DOWNLOAD_HOSTS: [&str; 3] = [
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
const MAX_BUILD_INFO_BYTES: u64 = 64 << 10;
const MAX_RECORD_BYTES: u64 = 4096;
/// One page of the releases API, with their notes and assets: a few hundred kilobytes today.
const MAX_LISTING_BYTES: usize = 8 << 20;
/// Releases asked for; the newest ones are all the Ports tab offers.
const RELEASES_PER_PAGE: u8 = 30;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the port publishes no Windows package")]
    NotPublished,
    #[error("could not reach GitHub: {0}")]
    Request(#[from] reqwest::Error),
    #[error("GitHub's rate limit for anonymous requests was reached")]
    RateLimited,
    #[error("GitHub answered something that isn't a list of releases: {0}")]
    MalformedListing(String),
    #[error("the release {0} has no checksum for its Windows package")]
    NoChecksum(String),
    #[error("{0}")]
    Download(#[from] bundle::Error),
    #[error("tar could not read the package: {0}")]
    Tar(String),
    #[error("the package holds {0}")]
    Layout(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl From<archive::Error> for Error {
    fn from(err: archive::Error) -> Self {
        match err {
            archive::Error::Tar(why) => Self::Tar(why),
            archive::Error::Layout(what) => Self::Layout(what),
            archive::Error::Io(err) => Self::Io(err),
        }
    }
}

/// A `vX.Y.Z` tag as [`prebuilt::valid_tag`] accepts it: it goes into URLs and a folder name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version(String);

#[derive(Debug, thiserror::Error)]
#[error("not a vX.Y.Z tag")]
pub struct InvalidVersion;

impl FromStr for Version {
    type Err = InvalidVersion;

    /// No leading zeros either, so that two tags of the same version can't both exist.
    fn from_str(tag: &str) -> Result<Self, Self::Err> {
        let canonical = tag
            .trim_start_matches('v')
            .split('.')
            .all(|part| part == "0" || !part.starts_with('0'));
        if prebuilt::valid_tag(tag) && canonical {
            Ok(Self(tag.to_owned()))
        } else {
            Err(InvalidVersion)
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers().cmp(&other.numbers())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Version {
    #[must_use]
    pub fn tag(&self) -> &str {
        &self.0
    }

    /// Each part has at most nine digits ([`prebuilt::valid_tag`]), so it fits.
    fn numbers(&self) -> [u32; 3] {
        let mut numbers = [0; 3];
        for (number, part) in numbers.iter_mut().zip(self.0.trim_start_matches('v').split('.')) {
            *number = part.parse().unwrap_or(0);
        }
        numbers
    }

    fn asset(&self) -> String {
        format!("psport-windows-{}-x86_64.zip", self.0)
    }
}

/// A release that carries the Windows package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    /// The package's bytes as its release lists them, checked again as it downloads.
    pub size: u64,
    /// `YYYY-MM-DD`, when the release gave a well-formed date.
    pub published: Option<String>,
}

/// A package in the library, as its record says it was downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: Version,
    pub sha256: String,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub downloaded_at: u64,
}

#[derive(Serialize, Deserialize)]
struct Record {
    version: String,
    asset: String,
    sha256: String,
    size: u64,
    downloaded_at: u64,
}

#[derive(Deserialize)]
struct ReleaseAnswer {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<AssetAnswer>,
}

#[derive(Deserialize)]
struct AssetAnswer {
    name: String,
    size: u64,
}

/// Every downloaded package, a folder per version, with its record beside it.
fn packages(root: &Root) -> PathBuf {
    root.join("psport-windows")
}

/// The package of `version`: `<library>/psport/psport-windows/<tag>`.
#[must_use]
pub fn dir(root: &Root, version: &Version) -> PathBuf {
    packages(root).join(version.tag())
}

fn record_path(root: &Root, version: &Version) -> PathBuf {
    packages(root).join(format!("{}.json", version.tag()))
}

/// The releases of `port` that carry the Windows package with its checksum, newest first. Tags
/// other than `vX.Y.Z`, such as the nix cache's, never count, as on Linux.
///
/// # Errors
/// When the port publishes none for this system, its releases API can't be reached or limits anonymous
/// requests, or its answer isn't a list of releases.
pub async fn offers(port: Port) -> Result<Vec<Offer>, Error> {
    let Some(repo) = port.github_repo().filter(|_| bundle::downloads(port).is_some()) else {
        return Err(Error::NotPublished);
    };
    let answers = match releases(repo).await {
        Ok(answers) => answers,
        Err(err) => return Err(err),
    };
    let mut offers: Vec<Offer> = answers
        .into_iter()
        .filter(|answer| !answer.draft && !answer.prerelease)
        .filter_map(|answer| {
            let version = answer.tag_name.parse::<Version>().ok()?;
            let asset = version.asset();
            let checksum = format!("{asset}.sha256");
            if !answer.assets.iter().any(|published| published.name == checksum) {
                return None;
            }
            let size = answer
                .assets
                .iter()
                .find(|published| published.name == asset)
                .map(|published| published.size)
                .filter(|&size| (1..=bundle::MAX_BYTES).contains(&size))?;
            let published = answer.published_at.as_deref().and_then(date_of);
            Some(Offer {
                version,
                size,
                published,
            })
        })
        .collect();
    offers.sort_by(|a, b| b.version.cmp(&a.version));
    offers.dedup_by(|a, b| a.version == b.version);
    tracing::debug!(port = port.name(), count = offers.len(), "Windows packages published");
    Ok(offers)
}

/// The date part of the API's `YYYY-MM-DDTHH:MM:SSZ`, nothing else of it.
fn date_of(timestamp: &str) -> Option<String> {
    let shape = b"dddd-dd-ddTdd:dd:ddZ";
    (timestamp.len() == shape.len()
        && timestamp.bytes().zip(shape).all(|(byte, &expected)| match expected {
            b'd' => byte.is_ascii_digit(),
            other => byte == other,
        }))
    .then(|| timestamp.chars().take(10).collect())
}

/// Redirects to [`DOWNLOAD_HOSTS`] and the releases API over HTTPS only, at most five: a download redirected anywhere
/// else fails rather than fetching from there. The [`bundle`] override's own URL is never a
/// redirect, so a local server still works through it.
pub(crate) fn github_only() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let allowed = attempt.url().scheme() == "https"
            && attempt
                .url()
                .host_str()
                .is_some_and(|host| DOWNLOAD_HOSTS.contains(&host) || host == "api.github.com");
        if attempt.previous().len() >= 5 {
            attempt.error("too many redirects")
        } else if allowed {
            attempt.follow()
        } else {
            tracing::warn!(url = %attempt.url(), "psport download redirected off GitHub");
            attempt.error("redirected off GitHub's release storage")
        }
    })
}

async fn releases(repo: &str) -> Result<Vec<ReleaseAnswer>, Error> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("decanter/", env!("CARGO_PKG_VERSION")))
        .redirect(github_only())
        .build()
    {
        Ok(client) => client,
        Err(err) => return Err(err.into()),
    };
    let response = match client
        .get(format!(
            "https://api.github.com/repos/{repo}/releases?per_page={RELEASES_PER_PAGE}"
        ))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(reqwest::header::CACHE_CONTROL, "no-cache")
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => return Err(err.into()),
    };
    let status = response.status();
    let exhausted = response
        .headers()
        .get("x-ratelimit-remaining")
        .is_some_and(|remaining| remaining.as_bytes() == b"0");
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || (status == reqwest::StatusCode::FORBIDDEN && exhausted) {
        tracing::warn!(repo, %status, "GitHub rate limit reached");
        return Err(Error::RateLimited);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        tracing::warn!(repo, "GitHub knows no such repository");
        return Ok(Vec::new());
    }
    let bytes = match response.error_for_status() {
        Ok(response) => response.bytes().await,
        Err(err) => Err(err),
    };
    match bytes {
        Ok(bytes) if bytes.len() > MAX_LISTING_BYTES => Err(Error::MalformedListing(format!("{} bytes", bytes.len()))),
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| Error::MalformedListing(err.to_string())),
        Err(err) => Err(err.into()),
    }
}

/// `offer`'s package, downloaded, checked and extracted to [`dir`], then recorded, reporting bytes
/// so far and the total when the server sends one. Nothing reaches [`dir`] unless every check passed.
///
/// # Errors
/// When the release has no well-formed checksum for the package, the download fails or doesn't match
/// its checksum and size, or the archive holds anything but the expected package.
pub async fn install(
    root: &Root,
    port: Port,
    offer: &Offer,
    on_download: impl FnMut(u64, Option<u64>),
) -> Result<Installed, Error> {
    let version = &offer.version;
    if let Some(installed) = installed_version(root, version).await {
        return Ok(installed);
    }
    let Some(downloads) = bundle::downloads(port) else {
        return Err(Error::NotPublished);
    };
    let asset = version.asset();
    let tag = version.tag();
    let text = match bundle::fetch_small(
        &format!("{downloads}/{tag}/{asset}.sha256"),
        bundle::MAX_CHECKSUM_BYTES,
        github_only(),
    )
    .await
    {
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        Ok(None) => return Err(Error::NoChecksum(tag.to_owned())),
        Err(err) => return Err(err.into()),
    };
    let Some(sha256) = bundle::checksum_of(&text, &asset) else {
        return Err(bundle::Error::MalformedChecksum {
            expected: asset,
            found: text.chars().take(200).collect(),
        }
        .into());
    };
    let parent = packages(root);
    if let Err(err) = tokio::fs::create_dir_all(&parent).await {
        return Err(err.into());
    }
    // Per process, so two decanter processes installing at once don't write into each other's; the
    // lock tells another process's leftovers, which [`sweep_stale`] removes, from its work in progress.
    let pid = std::process::id();
    let archive = parent.join(format!(".{tag}.{pid}.zip"));
    let staging = parent.join(format!(".{tag}.{pid}"));
    let lock_path = parent.join(format!(".{tag}.{pid}.lock"));
    let locked = {
        let (parent, lock_path) = (parent.clone(), lock_path.clone());
        tokio::task::spawn_blocking(move || {
            sweep_stale(&parent, pid);
            lock_install(&lock_path)
        })
        .await
    };
    let lock = match locked {
        Ok(Ok(lock)) => lock,
        Ok(Err(err)) => return Err(err.into()),
        Err(err) => return Err(io::Error::other(err).into()),
    };
    let target = dir(root, version);
    let url = format!("{downloads}/{tag}/{asset}");
    tracing::info!(
        port = port.name(),
        tag,
        url,
        size = offer.size,
        "downloading psport's Windows package"
    );
    let unpacked = match bundle::download(&url, &archive, &sha256, Some(offer.size), github_only(), on_download).await {
        Ok(()) => unpack(root, version, &archive, &staging, &target).await,
        Err(err) => Err(err.into()),
    };
    let _ = tokio::fs::remove_file(&archive).await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    drop(lock);
    let _ = tokio::fs::remove_file(&lock_path).await;
    match unpacked {
        Ok(None) => {}
        Ok(Some(installed)) => return Ok(installed),
        Err(err) => return Err(err),
    }
    let installed = Installed {
        version: version.clone(),
        sha256,
        size: offer.size,
        downloaded_at: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    };
    if let Err(err) = write_record(root, &installed, asset).await {
        return Err(err.into());
    }
    tracing::info!(
        port = port.name(),
        tag,
        sha256 = installed.sha256,
        dir = %target.display(),
        "psport's Windows package installed"
    );
    Ok(installed)
}

/// Written last, once the folder is in place: the record is what makes it count as installed.
async fn write_record(root: &Root, installed: &Installed, asset: String) -> io::Result<()> {
    let record = Record {
        version: installed.version.tag().to_owned(),
        asset,
        sha256: installed.sha256.clone(),
        size: installed.size,
        downloaded_at: installed.downloaded_at,
    };
    let text = match serde_json::to_vec_pretty(&record) {
        Ok(text) => text,
        Err(err) => return Err(io::Error::other(err)),
    };
    let path = record_path(root, &installed.version);
    match tokio::task::spawn_blocking(move || crate::files::write_atomic(&path, &text)).await {
        Ok(result) => result,
        Err(err) => Err(io::Error::other(err)),
    }
}

/// Extracts the archive into `staging` with every check of [`archive::unpack`], then
/// [`check_programs`], before its folder is renamed to `target`. The version another process installed
/// meanwhile is kept as it is and returned instead.
async fn unpack(
    root: &Root,
    version: &Version,
    archive: &Path,
    staging: &Path,
    target: &Path,
) -> Result<Option<Installed>, Error> {
    let (top, files) = match archive::unpack(archive, staging, Top::AnyFolder, Names::Plain, LIMITS).await {
        Ok(unpacked) => (unpacked.dir, unpacked.files),
        Err(err) => return Err(err.into()),
    };
    let checked = {
        let top = top.clone();
        tokio::task::spawn_blocking(move || check_programs(&top, &files)).await
    };
    match checked {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(err),
        Err(err) => return Err(io::Error::other(err).into()),
    }
    if let Some(installed) = installed_version(root, version).await {
        return Ok(Some(installed));
    }
    // Only what an interrupted install left: a folder with a valid record was returned above.
    let _ = tokio::fs::remove_dir_all(target).await;
    match tokio::fs::rename(&top, target).await {
        Ok(()) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Removes what installs of other processes left in `parent` (`.<tag>.<pid>`, its `.zip` and
/// `.lock`) unless that process still holds its lock; never this process's own. Best effort, said
/// in the log. Blocking file I/O.
fn sweep_stale(parent: &Path, pid: u32) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let mut owners = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|name| name.strip_prefix('.')) else {
            continue;
        };
        let stem = rest
            .strip_suffix(".zip")
            .or_else(|| rest.strip_suffix(".lock"))
            .unwrap_or(rest);
        let Some((tag, owner)) = stem.rsplit_once('.') else {
            continue;
        };
        if tag.parse::<Version>().is_ok()
            && let Ok(owner) = owner.parse::<u32>()
            && owner != pid
        {
            owners.insert((tag.to_owned(), owner));
        }
    }
    for (tag, owner) in owners {
        let lock_path = parent.join(format!(".{tag}.{owner}.lock"));
        let held = File::options()
            .write(true)
            .open(&lock_path)
            .is_ok_and(|lock| matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)));
        if held {
            continue;
        }
        let staging = parent.join(format!(".{tag}.{owner}"));
        let removed = [
            std::fs::remove_dir_all(&staging),
            std::fs::remove_file(parent.join(format!(".{tag}.{owner}.zip"))),
            std::fs::remove_file(&lock_path),
        ];
        for result in removed {
            match result {
                Err(err) if err.kind() != io::ErrorKind::NotFound => {
                    tracing::warn!(tag, owner, %err, "could not remove an interrupted package install");
                }
                Ok(()) | Err(_) => {}
            }
        }
        tracing::info!(tag, owner, "interrupted package install of another process removed");
    }
}

/// This install's lock, held until it ends; the system drops it with the process.
pub(crate) fn lock_install(path: &Path) -> io::Result<File> {
    let lock = File::create(path)?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("{} is held by another install", path.display()),
        )),
        Err(std::fs::TryLockError::Error(err)) => Err(err),
    }
}

/// The programs are the known four ([`PROGRAMS`] and [`GPU_LIBRARY`]), the DLLs the ones its
/// `BUILD-INFO.txt` lists, every one of those there and beside `psport.exe`, the testers' scripts
/// ([`SCRIPTS`]) the only other runnable files, and the rest texts. Blocking file I/O.
pub(crate) fn check_programs(top: &Path, files: &BTreeMap<String, u64>) -> Result<(), Error> {
    if !files.contains_key(BUILD_INFO) {
        return Err(Error::Layout(format!("no {BUILD_INFO}")));
    }
    let listed = match read_capped(&top.join(BUILD_INFO), MAX_BUILD_INFO_BYTES) {
        Ok(text) => listed_dlls(&text),
        Err(err) => return Err(err.into()),
    };
    let Some(listed) = listed else {
        return Err(Error::Layout(format!("a {BUILD_INFO} without its list of DLLs")));
    };
    for file in files.keys() {
        let extension = file
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        let is_top_level = !file.contains('/');
        let allowed = match extension.as_str() {
            "exe" => is_top_level && PROGRAMS.contains(&file.as_str()),
            "dll" => is_top_level && (file == GPU_LIBRARY || listed.iter().any(|dll| dll.eq_ignore_ascii_case(file))),
            _ if SCRIPTS.contains(&file.as_str()) => true,
            other => DOCUMENTS.contains(&other),
        };
        if !allowed {
            return Err(Error::Layout(format!("{file}, which isn't part of psport's package")));
        }
    }
    let required = [EXECUTABLE, GPU_LIBRARY].into_iter().map(str::to_owned).chain(listed);
    for file in required {
        if !files.keys().any(|present| present.eq_ignore_ascii_case(&file)) {
            return Err(Error::Layout(format!("no {file}")));
        }
    }
    Ok(())
}

/// The DLLs under `DLLs included (N):`, two-space indented and first on their line, N of them.
fn listed_dlls(text: &str) -> Option<Vec<String>> {
    let mut lines = text.lines();
    let count: usize = lines
        .find_map(|line| line.strip_prefix("DLLs included ("))?
        .strip_suffix("):")?
        .parse()
        .ok()?;
    let mut dlls = Vec::with_capacity(count);
    for line in lines.take(count) {
        let dll = line.strip_prefix("  ")?.split(' ').next()?;
        let is_dll = dll
            .rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("dll"));
        if !archive::valid_component(dll, Names::Plain) || !is_dll {
            return None;
        }
        dlls.push(dll.to_owned());
    }
    (dlls.len() == count).then_some(dlls)
}

/// Blocking file I/O.
fn read_capped(path: &Path, cap: u64) -> io::Result<String> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a file under {cap} bytes", path.display()),
        ));
    }
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Every package in the library with a valid record and its programs in place, newest first.
pub async fn installed(root: &Root) -> Vec<Installed> {
    let Ok(mut entries) = tokio::fs::read_dir(packages(root)).await else {
        return Vec::new();
    };
    let mut found = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let Some(version) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|tag| tag.parse::<Version>().ok())
        else {
            continue;
        };
        if let Some(installed) = installed_version(root, &version).await {
            found.push(installed);
        }
    }
    found.sort_by(|a, b| b.version.cmp(&a.version));
    found
}

/// `version`'s package when its record reads back as written and its programs are files of their
/// own: a record is written only after the folder passed every check.
pub async fn installed_version(root: &Root, version: &Version) -> Option<Installed> {
    let path = record_path(root, version);
    let record = match tokio::fs::symlink_metadata(&path).await {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_RECORD_BYTES => tokio::fs::read(&path).await.ok()?,
        Ok(_) | Err(_) => return None,
    };
    let record: Record = match serde_json::from_slice(&record) {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "unreadable record of a Windows package");
            return None;
        }
    };
    let sound = record.version == version.tag()
        && record.asset == version.asset()
        && record.sha256.len() == 64
        && record
            .sha256
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && record.size <= bundle::MAX_BYTES;
    if !sound {
        tracing::warn!(path = %path.display(), "record of a Windows package that doesn't match its name");
        return None;
    }
    let dir = dir(root, version);
    for file in [EXECUTABLE, GPU_LIBRARY] {
        if !tokio::fs::symlink_metadata(dir.join(file))
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            return None;
        }
    }
    Some(Installed {
        version: version.clone(),
        sha256: record.sha256,
        size: record.size,
        downloaded_at: record.downloaded_at,
    })
}

/// `version`'s record first, so a folder half removed never reads as installed.
///
/// # Errors
/// When either can't be removed; a missing one is not an error.
pub async fn remove(root: &Root, version: &Version) -> io::Result<()> {
    match tokio::fs::remove_file(record_path(root, version)).await {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    match tokio::fs::remove_dir_all(dir(root, version)).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}
