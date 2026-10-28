//! GE-Proton's archive, checked before and after the host's `tar` extracts it into a staging
//! folder: before, every name it lists must sit in the one folder named as the tag, with no `.`,
//! `..`, empty or control-character component; after, the staging folder must hold only that
//! folder, of plain files, folders and links that resolve to something inside it, within the entry
//! and size caps, with `proton` and `toolmanifest.vdf` there. Nothing from the archive runs here.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context as _, bail};
use tokio::process::Command;

use super::REQUIRED;

/// Measured for GE-Proton11-9 unpacked: 8,931 entries, 1.6 GB, the largest file 72 MB; the caps
/// leave room for later releases, not for another kind of archive.
const MAX_ENTRIES: usize = 32_000;
const MAX_UNPACKED_BYTES: u64 = 4 << 30;
const MAX_FILE_BYTES: u64 = 512 << 20;

pub(super) async fn unpack(archive: &Path, staging: &Path, tag: &str) -> anyhow::Result<()> {
    let listing = tar(&[
        OsStr::new("--list"),
        OsStr::new("--gzip"),
        OsStr::new("--file"),
        archive.as_os_str(),
    ])
    .await
    .context("listing GE-Proton's archive")?;
    if let Err(why) = check_listing(&listing, tag) {
        bail!("GE-Proton's archive holds {why}");
    }
    if let Err(err) = tokio::fs::remove_dir_all(staging).await
        && err.kind() != io::ErrorKind::NotFound
    {
        return Err(err).context("clearing GE-Proton's staging folder");
    }
    tokio::fs::create_dir_all(staging)
        .await
        .context("creating GE-Proton's staging folder")?;
    tar(&[
        OsStr::new("--extract"),
        OsStr::new("--gzip"),
        OsStr::new("--no-same-owner"),
        OsStr::new("--file"),
        archive.as_os_str(),
        OsStr::new("--directory"),
        staging.as_os_str(),
    ])
    .await
    .context("extracting GE-Proton's archive")?;
    let checked = {
        let (staging, top) = (staging.to_owned(), staging.join(tag));
        tokio::task::spawn_blocking(move || check_tree(&staging, &top)).await
    };
    match checked {
        Ok(Ok(())) => {}
        Ok(Err(why)) => bail!("GE-Proton's archive holds {why}"),
        Err(err) => return Err(err).context("checking GE-Proton's files"),
    }
    let top = staging.join(tag);
    if let Some(missing) = REQUIRED.iter().find(|file| !top.join(file).is_file()) {
        bail!("GE-Proton's archive holds no {missing}");
    }
    Ok(())
}

/// The host's `tar`, as `umu-run`'s install uses it, rather than an archive crate for one archive
/// per release. Its output as text; nothing it reads is ever run.
async fn tar(args: &[&OsStr]) -> anyhow::Result<String> {
    let mut command = Command::new("tar");
    command
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    decanter_core::strip_launcher_env(command.as_std_mut());
    let output = command.output().await.context("spawning tar")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("tar: {}", stderr.lines().last().unwrap_or("failed"));
    }
    String::from_utf8(output.stdout).context("tar listed names that aren't UTF-8")
}

/// Every name `tar --list` printed sits in the folder `tag` and is made of [`plain_name`]s. `tar`
/// prints a name with a byte it can't show escaped by a backslash, which [`plain_name`] refuses.
fn check_listing(listing: &str, tag: &str) -> Result<(), String> {
    let mut entries: usize = 0;
    for name in listing.lines() {
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(format!("more than {MAX_ENTRIES} entries"));
        }
        let shown: String = name.chars().take(200).collect();
        let mut components = name.strip_suffix('/').unwrap_or(name).split('/');
        if components.next() != Some(tag) {
            return Err(format!("{shown} outside its {tag} folder"));
        }
        if !components.all(plain_name) {
            return Err(format!("the name {shown}"));
        }
    }
    if entries == 0 {
        return Err("nothing".to_owned());
    }
    Ok(())
}

fn plain_name(name: &str) -> bool {
    (1..=255).contains(&name.len())
        && name != "."
        && name != ".."
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
}

/// `staging` holds `top` alone, and `top` only folders, plain files and links that resolve to
/// something inside it, within the caps. Blocking file I/O.
fn check_tree(staging: &Path, top: &Path) -> Result<(), String> {
    let shown = |path: &Path| path.strip_prefix(staging).unwrap_or(path).display().to_string();
    for read in std::fs::read_dir(staging).map_err(|err| err.to_string())? {
        let entry = read.map_err(|err| err.to_string())?;
        if entry.path() != top {
            return Err(format!("{} beside its folder", shown(&entry.path())));
        }
    }
    let real_top = std::fs::canonicalize(top).map_err(|err| format!("no folder: {err}"))?;
    let mut count: usize = 0;
    let mut unpacked: u64 = 0;
    let mut pending = vec![top.to_owned()];
    while let Some(dir) = pending.pop() {
        for read in std::fs::read_dir(&dir).map_err(|err| err.to_string())? {
            let entry = read.map_err(|err| err.to_string())?;
            count += 1;
            if count > MAX_ENTRIES {
                return Err(format!("more than {MAX_ENTRIES} entries"));
            }
            // Not followed: a link is the entry itself.
            let kind = entry.file_type().map_err(|err| err.to_string())?;
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_symlink() {
                // Resolved in full, since a link through another link can leave by `..` what each
                // link's own text keeps inside.
                let inside = std::fs::canonicalize(&path).is_ok_and(|real| real.starts_with(&real_top));
                if !inside {
                    return Err(format!("a link leaving its folder: {}", shown(&path)));
                }
            } else if kind.is_file() {
                let size = entry.metadata().map_err(|err| err.to_string())?.len();
                unpacked = unpacked.saturating_add(size);
                if size > MAX_FILE_BYTES || unpacked > MAX_UNPACKED_BYTES {
                    return Err(format!("{}, too large unpacked ({size} bytes)", shown(&path)));
                }
            } else {
                return Err(format!("a special file: {}", shown(&path)));
            }
        }
    }
    Ok(())
}
