//! A release of the port as its CI bundles it (psport's `tools/bundle.sh`): the programs with every
//! library they link, glibc's loader included, run from the library by the host's bash with the
//! host's Vulkan drivers, and no nix anywhere. Release runs a tag's bundle when CI published one;
//! tags from before the bundles only have the nix cache ([`crate::os::linux::prebuilt`]).
//!
//! A bundle is checked where it enters: its checksum file is the tag release's own, the archive
//! must match it and stay under [`download::MAX_BYTES`], and what `tar` extracts must be one folder of plain
//! files, folders and links that stay inside it. Only then is it renamed into place, so a folder
//! at [`dir`] is trusted as is.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use tokio::process::Command;

use crate::download::{self, Error, MAX_CHECKSUM_BYTES, checksum_of, downloads, fetch_small};
use crate::paths::Root;
use crate::port::Port;

/// What `run.sh` and decanter run, without which the folder is no bundle.
const REQUIRED: [&str; 5] = [
    "run.sh",
    "relocate.sh",
    "out/psport",
    "out/psport-gpu-capabilities",
    "lib/ld-linux-x86-64.so.2",
];

fn asset(tag: &str) -> String {
    format!("psport-{tag}-x86_64.tar.zst")
}

/// Every bundle of `port` in the library, a folder per tag beside its checkout: inside it, a
/// checkout reset or clean would take them along.
fn bundles(root: &Root, port: Port) -> PathBuf {
    port.checkout(root)
        .with_file_name(format!("{}-bundles", port.name().to_lowercase()))
}

/// The bundle of `tag`, a tag [`crate::os::linux::prebuilt::valid_tag`] accepts.
pub fn dir(root: &Root, port: Port, tag: &str) -> PathBuf {
    bundles(root, port).join(tag)
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
    let installed = match download::download(
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
    crate::files::remove_entries(&bundles(root, port), "old port bundle", |name| name != keep).await;
}
