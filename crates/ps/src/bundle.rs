//! A release of the port as its CI bundles it (psport's `tools/bundle.sh`): the programs with every
//! library they link, glibc's loader included, run from the library by the host's bash with the
//! host's Vulkan drivers, and no nix anywhere. Release runs a tag's bundle when CI published one;
//! tags from before the bundles only have the nix cache ([`crate::prebuilt`]).
//!
//! A bundle is checked where it enters: its checksum file is the tag release's own, the archive
//! must match it and stay under [`MAX_BYTES`], and what `tar` extracts must be one folder of plain
//! files, folders and links that stay inside it. Only then is it renamed into place, so a folder
//! at [`dir`] is trusted as is.

use std::fmt::Write as _;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use decanter_core::capabilities::Feature;
use decanter_core::platform;
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

use crate::paths::Root;
use crate::port::Port;

/// Lower-case hex of a digest, the form checksum files and the installed record carry.
pub(crate) fn hex_digest(digest: &[u8]) -> String {
    digest
        .iter()
        .fold(String::with_capacity(digest.len() * 2), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// Points every port's bundles somewhere else, a local `http://` server or a `file://` folder
/// laid out as `<tag>/<asset>`, to try a bundle before CI publishes it.
const DOWNLOADS_OVERRIDE: &str = "DECANTER_PS_BUNDLES";

/// About 50 MB today: the cap only stops a wrong or runaway download.
pub(crate) const MAX_BYTES: u64 = 512 << 20;

pub(crate) const MAX_CHECKSUM_BYTES: usize = 4096;

/// What `run.sh` and decanter run, without which the folder is no bundle.
const REQUIRED: [&str; 5] = [
    "run.sh",
    "relocate.sh",
    "out/psport",
    "out/psport-gpu-capabilities",
    "lib/ld-linux-x86-64.so.2",
];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not download the bundle: {0}")]
    Download(#[from] reqwest::Error),
    #[error("the bundle is larger than the {MAX_BYTES}-byte cap: {0} bytes")]
    TooLarge(u64),
    #[error("the bundle's checksum file is not `<sha256>  {expected}`: {found:?}")]
    MalformedChecksum { expected: String, found: String },
    #[error("the bundle does not match its published checksum")]
    Checksum,
    #[error("the download is {found} bytes, not the {expected} published")]
    Size { expected: u64, found: u64 },
    #[error("tar could not extract the bundle: {0}")]
    Extract(String),
    #[error("the bundle holds {0}")]
    Layout(String),
    #[error("the bundle could not be pointed at its own loader: {0}")]
    Relocate(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

fn asset(tag: &str) -> String {
    format!("psport-{tag}-x86_64.tar.zst")
}

/// Where the published bundles of `port` are, when its CI publishes any for this machine.
pub(crate) fn downloads(port: Port) -> Option<String> {
    if !platform::support(Feature::PsDownloads).is_offered() {
        return None;
    }
    port.release_downloads()
        .map(|published| std::env::var(DOWNLOADS_OVERRIDE).unwrap_or_else(|_| published.to_owned()))
}

/// Every bundle of `port` in the library, a folder per tag beside its checkout: inside it, a
/// checkout reset or clean would take them along.
fn bundles(root: &Root, port: Port) -> PathBuf {
    port.checkout(root)
        .with_file_name(format!("{}-bundles", port.name().to_lowercase()))
}

/// The bundle of `tag`, a tag [`crate::prebuilt::valid_tag`] accepts.
pub fn dir(root: &Root, port: Port, tag: &str) -> PathBuf {
    bundles(root, port).join(tag)
}

/// The file at `url`, `None` when there is none, and an error past `cap` bytes, following the
/// redirects `redirect` allows.
pub(crate) async fn fetch_small(
    url: &str,
    cap: usize,
    redirect: reqwest::redirect::Policy,
) -> Result<Option<Vec<u8>>, Error> {
    if let Some(path) = url.strip_prefix("file://") {
        return match tokio::fs::read(path).await {
            Ok(bytes) if bytes.len() > cap => Err(Error::TooLarge(bytes.len() as u64)),
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        };
    }
    // Bounded, since every preparation of the port on Release waits on it.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(redirect)
        .build()
    {
        Ok(client) => client,
        Err(err) => return Err(err.into()),
    };
    let response = match client.get(url).send().await {
        Ok(response) if response.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
        Ok(response) => response.error_for_status(),
        Err(err) => Err(err),
    };
    let bytes = match response {
        Ok(response) => response.bytes().await,
        Err(err) => Err(err),
    };
    match bytes {
        Ok(bytes) if bytes.len() > cap => Err(Error::TooLarge(bytes.len() as u64)),
        Ok(bytes) => Ok(Some(bytes.to_vec())),
        Err(err) => Err(err.into()),
    }
}

/// A bundle CI published: where it is and the SHA-256 published beside it.
pub struct Published {
    url: String,
    sha256: String,
}

/// `tag`'s bundle, `None` when the tag has none (released before CI made them, or its upload not
/// done yet) or `port` publishes none at all.
pub async fn published(port: Port, tag: &str) -> Result<Option<Published>, Error> {
    let Some(downloads) = downloads(port) else {
        return Ok(None);
    };
    let expected = asset(tag);
    let text = match fetch_small(
        &format!("{downloads}/{tag}/{expected}.sha256"),
        MAX_CHECKSUM_BYTES,
        reqwest::redirect::Policy::default(),
    )
    .await
    {
        Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        Ok(None) => return Ok(None),
        Err(err) => return Err(err),
    };
    match checksum_of(&text, &expected) {
        Some(sha256) => Ok(Some(Published {
            url: format!("{downloads}/{tag}/{expected}"),
            sha256,
        })),
        None => Err(Error::MalformedChecksum {
            expected,
            found: text.chars().take(200).collect(),
        }),
    }
}

/// The SHA-256 a `sha256sum` line gives for `asset`: `<sha256>  <asset>`, nothing else.
pub(crate) fn checksum_of(text: &str, asset: &str) -> Option<String> {
    let mut parts = text.split_whitespace();
    match (parts.next(), parts.next(), parts.next()) {
        (Some(sum), Some(name), None)
            if sum.len() == 64
                && sum.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                && name.strip_prefix('*').unwrap_or(name) == asset =>
        {
            Some(sum.to_owned())
        }
        _ => None,
    }
}

/// Whether `tag`'s bundle is in the library: only a checked one is ever renamed into place.
pub async fn is_installed(root: &Root, port: Port, tag: &str) -> bool {
    let dir = dir(root, port, tag);
    for file in REQUIRED {
        if !tokio::fs::metadata(dir.join(file))
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            return false;
        }
    }
    true
}

/// Downloads `tag`'s bundle as [`published`] found it, checks it against its checksum and
/// extracts it to [`dir`], reporting bytes so far and the total when the server sends one.
/// Nothing reaches [`dir`] unless every check passed.
pub async fn install(
    root: &Root,
    port: Port,
    tag: &str,
    published: &Published,
    on_download: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, Error> {
    let target = dir(root, port, tag);
    if is_installed(root, port, tag).await {
        return Ok(target);
    }
    let parent = bundles(root, port);
    if let Err(err) = tokio::fs::create_dir_all(&parent).await {
        return Err(err.into());
    }
    // Per process, so two decanter processes installing at once don't write into each other's.
    let archive = parent.join(format!(".{tag}.{}.tar.zst", std::process::id()));
    let staging = parent.join(format!(".{tag}.{}", std::process::id()));
    tracing::info!(
        port = port.name(),
        tag,
        url = published.url,
        "downloading the port's bundle"
    );
    let installed = match download(
        &published.url,
        &archive,
        &published.sha256,
        None,
        reqwest::redirect::Policy::default(),
        on_download,
    )
    .await
    {
        Ok(()) => unpack(&archive, &staging, tag, &target).await,
        Err(err) => Err(err),
    };
    let _ = tokio::fs::remove_file(&archive).await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    match installed {
        Ok(()) => {
            tracing::info!(port = port.name(), tag, dir = %target.display(), "port bundle installed");
            Ok(target)
        }
        Err(err) => Err(err),
    }
}

/// `size`, when the publisher gives one, is checked as the bytes arrive, before the checksum.
pub(crate) async fn download(
    url: &str,
    archive: &Path,
    sha256: &str,
    size: Option<u64>,
    redirect: reqwest::redirect::Policy,
    mut on_download: impl FnMut(u64, Option<u64>),
) -> Result<(), Error> {
    if let Some(path) = url.strip_prefix("file://") {
        let found = match tokio::fs::metadata(path).await {
            Ok(meta) => meta.len(),
            Err(err) => return Err(err.into()),
        };
        if found > MAX_BYTES {
            return Err(Error::TooLarge(found));
        }
        if let Some(expected) = size.filter(|&expected| expected != found) {
            return Err(Error::Size { expected, found });
        }
        let size = found;
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(err) => return Err(err.into()),
        };
        on_download(size, Some(size));
        if hex_digest(&Sha256::digest(&bytes)) != sha256 {
            return Err(Error::Checksum);
        }
        return match tokio::fs::write(archive, &bytes).await {
            Ok(()) => Ok(()),
            Err(err) => Err(err.into()),
        };
    }
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .redirect(redirect)
        .build()
    {
        Ok(client) => client,
        Err(err) => return Err(err.into()),
    };
    let mut response = match client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    {
        Ok(response) => response,
        Err(err) => return Err(err.into()),
    };
    let total = response.content_length();
    if let Some(total) = total.filter(|&total| total > MAX_BYTES) {
        return Err(Error::TooLarge(total));
    }
    if let (Some(expected), Some(found)) = (size, total)
        && expected != found
    {
        return Err(Error::Size { expected, found });
    }
    let mut file = match tokio::fs::File::create(archive).await {
        Ok(file) => file,
        Err(err) => return Err(err.into()),
    };
    let mut hasher = Sha256::new();
    let mut received = 0;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(err) => return Err(err.into()),
        };
        received += chunk.len() as u64;
        if received > MAX_BYTES {
            return Err(Error::TooLarge(received));
        }
        if let Some(expected) = size.filter(|&expected| received > expected) {
            return Err(Error::Size {
                expected,
                found: received,
            });
        }
        hasher.update(&chunk);
        if let Err(err) = file.write_all(&chunk).await {
            return Err(err.into());
        }
        on_download(received, total);
    }
    if let Err(err) = file.flush().await {
        return Err(err.into());
    }
    if let Some(expected) = size.filter(|&expected| received != expected) {
        return Err(Error::Size {
            expected,
            found: received,
        });
    }
    if hex_digest(&hasher.finalize()) != sha256 {
        return Err(Error::Checksum);
    }
    Ok(())
}

/// The host's `tar` and `zstd`, as decanter's umu-run download uses its `tar`, rather than an
/// archive crate for one file per release.
async fn unpack(archive: &Path, staging: &Path, tag: &str, target: &Path) -> Result<(), Error> {
    let _ = tokio::fs::remove_dir_all(staging).await;
    if let Err(err) = tokio::fs::create_dir_all(staging).await {
        return Err(err.into());
    }
    let mut command = Command::new("tar");
    command
        .args(["--zstd", "--extract", "--no-same-owner", "--file"])
        .arg(archive)
        .arg("--directory")
        .arg(staging)
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    let output = match command.output().await {
        Ok(output) => output,
        Err(err) => return Err(Error::Extract(format!("tar: {err}"))),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Extract(stderr.lines().last().unwrap_or("failed").to_owned()));
    }
    let top = staging.join(format!("psport-{tag}-x86_64"));
    let checked_staging = staging.to_owned();
    let checked_top = top.clone();
    let checked = tokio::task::spawn_blocking(move || check_tree(&checked_staging, &checked_top)).await;
    match checked {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(err),
        Err(err) => return Err(Error::Io(io::Error::other(err))),
    }
    for file in REQUIRED {
        if !tokio::fs::metadata(top.join(file))
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            return Err(Error::Layout(format!("no {file}")));
        }
    }
    let _ = tokio::fs::remove_dir_all(target).await;
    match tokio::fs::rename(&top, target).await {
        Ok(()) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// One folder, `top`, of regular files, folders and relative links that stay below their own
/// folder. Blocking file I/O.
fn check_tree(staging: &Path, top: &Path) -> Result<(), Error> {
    let entries = match std::fs::read_dir(staging) {
        Ok(entries) => entries,
        Err(err) => return Err(err.into()),
    };
    for entry in entries {
        match entry {
            Ok(entry) if entry.path() == top => {}
            Ok(entry) => {
                return Err(Error::Layout(format!(
                    "{} beside its folder",
                    entry.file_name().to_string_lossy()
                )));
            }
            Err(err) => return Err(err.into()),
        }
    }
    let mut pending = vec![top.to_owned()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) => return Err(err.into()),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => return Err(err.into()),
            };
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(err) => return Err(err.into()),
            };
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_symlink() {
                let link = match std::fs::read_link(&path) {
                    Ok(link) => link,
                    Err(err) => return Err(err.into()),
                };
                if !link
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                {
                    return Err(Error::Layout(format!("a link leaving its folder: {}", path.display())));
                }
            } else if !kind.is_file() {
                return Err(Error::Layout(format!("a special file: {}", path.display())));
            }
        }
    }
    Ok(())
}

/// Points the bundle's programs at its own glibc loader for wherever the library now is (its
/// `relocate.sh`, which `run.sh` runs too): needed before anything starts them directly.
pub async fn relocate(dir: &Path) -> Result<(), Error> {
    let mut command = Command::new("bash");
    command
        .arg("relocate.sh")
        .current_dir(dir)
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    decanter_core::strip_launcher_env(command.as_std_mut());
    match command.output().await {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(Error::Relocate(
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("failed")
                .to_owned(),
        )),
        Err(err) => Err(Error::Relocate(err.to_string())),
    }
}

/// The bundles of tags other than `keep`, and what an interrupted install left: a bundle is only
/// run for the checkout's own tag. Best effort, said in the log.
pub async fn remove_others(root: &Root, port: Port, keep: &str) {
    let Ok(mut entries) = tokio::fs::read_dir(bundles(root, port)).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.file_name() == keep {
            continue;
        }
        let path = entry.path();
        let removed = match entry.file_type().await {
            Ok(kind) if kind.is_dir() => tokio::fs::remove_dir_all(&path).await,
            Ok(_) => tokio::fs::remove_file(&path).await,
            Err(err) => Err(err),
        };
        match removed {
            Ok(()) => tracing::info!(port = port.name(), path = %path.display(), "old port bundle removed"),
            Err(err) => {
                tracing::warn!(port = port.name(), path = %path.display(), %err, "could not remove an old port bundle")
            }
        }
    }
}
