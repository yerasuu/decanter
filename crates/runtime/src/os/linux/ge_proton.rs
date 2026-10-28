//! decanter's own GE-Proton: one pinned release, fetched from its `GitHub` release, checked against
//! the pin and unpacked into the library, for what needs a Proton build without Steam (psport's
//! FSR 4.1.1 recording). Games keep the builds umu downloads itself.
//!
//! Before `tar` extracts anything every name it lists must sit in the one folder named as the tag,
//! with no `.`, `..`, empty or control-character component; what lands in the staging folder must
//! be only that folder, of plain files, folders and links that resolve to something inside it,
//! within the entry and size caps; then `proton` and `toolmanifest.vdf` must be there. Only then
//! the folder is renamed into place. Nothing from the archive runs here.

use std::ffi::OsStr;
use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context as _, bail};
use decanter_core::runner::{Installed, Origin, Pin, Progress, Stage, Status, Tag};
use sha2::Sha512;
use tokio::process::Command;

use crate::download::{self, Expected};

const REPO: &str = "GloriousEggroll/proton-ge-custom";

/// The one GE-Proton release decanter installs: pinned like `umu-run` and bumped only after
/// psport's FSR 4.1.1 recording ran under it, since psport measured that some Proton builds don't
/// start the DLL's FSR 4.1 at all (GE-Proton 11-6 does, 2026-10-07). `sha512` is the release's own
/// `<tag>.sha512sum` and `size` its `<tag>.tar.gz` asset's bytes; until both are filled in every
/// install refuses.
const GE_PROTON: GeProtonPin = GeProtonPin {
    tag: "GE-Proton11-6",
    sha512: "",
    size: 0,
};

struct GeProtonPin {
    tag: &'static str,
    /// Lower-case hex SHA-512 of `<tag>.tar.gz`.
    sha512: &'static str,
    size: u64,
}

/// A GE-Proton archive is about half a gigabyte: the cap only stops a wrong or runaway download.
const MAX_ARCHIVE_BYTES: u64 = 1 << 30;
/// Measured for GE-Proton11-9 unpacked: 8,931 entries, 1.6 GB, the largest file 72 MB; the caps
/// leave room for later releases, not for another kind of archive.
const MAX_ENTRIES: usize = 32_000;
const MAX_UNPACKED_BYTES: u64 = 4 << 30;
const MAX_FILE_BYTES: u64 = 512 << 20;

/// What umu and psport's script read from a Proton folder.
const REQUIRED: [&str; 2] = ["proton", "toolmanifest.vdf"];

/// Held by the one install running, so the leftovers of any other (`.`-named) are a crash's.
const LOCK: &str = ".lock";

fn folder() -> Option<PathBuf> {
    decanter_core::bootstrap::data_dir().map(|data_dir| data_dir.join("runners").join("ge-proton"))
}

fn is_installed(dir: &Path) -> bool {
    REQUIRED.iter().all(|file| dir.join(file).is_file())
}

/// Every complete GE-Proton release in the active library's `runners/ge-proton` folder.
pub(crate) fn ge_proton_status() -> Status {
    let Some(entries) = folder().and_then(|folder| std::fs::read_dir(folder).ok()) else {
        return Status::Absent;
    };
    let mut installed: Vec<Installed> = entries
        .flatten()
        .filter_map(|entry| {
            let tag = entry
                .file_name()
                .to_str()
                .filter(|name| !name.starts_with('.'))
                .and_then(Tag::try_new)?;
            let path = entry.path();
            is_installed(&path).then_some(Installed {
                pin: Pin::Release(tag),
                origin: Origin::Download,
                path,
            })
        })
        .collect();
    if installed.is_empty() {
        return Status::Absent;
    }
    installed.sort_by(|left, right| left.path.cmp(&right.path));
    Status::Installed(installed)
}

/// Installs GE-Proton at `pin` unless it is there; only [`GE_PROTON`] is accepted.
pub(crate) async fn ge_proton_prepare(
    pin: &Pin,
    on_progress: &mut (dyn FnMut(Progress) + Send),
) -> anyhow::Result<Installed> {
    let Pin::Release(tag) = pin;
    if tag.as_str() != GE_PROTON.tag {
        bail!("GE-Proton {tag} is not the pinned {}", GE_PROTON.tag);
    }
    let Some(folder) = folder() else {
        bail!("the active library isn't available");
    };
    let path = folder.join(tag.as_str());
    if !is_installed(&path) {
        install(&folder, &path, on_progress).await?;
    }
    Ok(Installed {
        pin: Pin::Release(tag.clone()),
        origin: Origin::Download,
        path,
    })
}

/// Installs [`GE_PROTON`] and, once it is newly in, removes the releases it replaces.
pub(crate) async fn ge_proton_update(on_progress: &mut (dyn FnMut(Progress) + Send)) -> anyhow::Result<Installed> {
    let Some(tag) = Tag::try_new(GE_PROTON.tag) else {
        bail!("GE-Proton's pinned tag {:?} is no release name", GE_PROTON.tag);
    };
    let was_installed = folder().is_some_and(|folder| is_installed(&folder.join(tag.as_str())));
    let installed = ge_proton_prepare(&Pin::Release(tag), on_progress).await?;
    if !was_installed {
        remove_other_versions(&installed.path).await;
    }
    Ok(installed)
}

/// Removes `pin`'s folder from the active library.
pub(crate) async fn ge_proton_remove(pin: &Pin) -> io::Result<()> {
    let Some(folder) = folder() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the active library isn't available",
        ));
    };
    let Pin::Release(tag) = pin;
    let path = folder.join(tag.as_str());
    match tokio::fs::remove_dir_all(&path).await {
        Ok(()) => {
            tracing::info!(path = %path.display(), "GE-Proton removed");
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

async fn install(folder: &Path, target: &Path, on_progress: &mut (dyn FnMut(Progress) + Send)) -> anyhow::Result<()> {
    let pin = &GE_PROTON;
    if !download::is_hex_digest(pin.sha512, 64) || pin.size == 0 {
        bail!("GE-Proton {} has no pinned SHA-512 and size", pin.tag);
    }
    tokio::fs::create_dir_all(folder)
        .await
        .context("creating the library's GE-Proton folder")?;
    let lock = lock(folder).await?;
    if is_installed(target) {
        return Ok(());
    }
    let pid = std::process::id();
    let archive = folder.join(format!(".{}.{pid}.tar.gz", pin.tag));
    let staging = folder.join(format!(".{}.{pid}.part", pin.tag));
    let leftovers = Leftovers(vec![archive.clone(), staging.clone()]);
    let url = format!(
        "https://github.com/{REPO}/releases/download/{tag}/{tag}.tar.gz",
        tag = pin.tag
    );
    tracing::info!(url, "downloading GE-Proton");
    on_progress(Progress {
        stage: Stage::Downloading,
        received: 0,
        total: Some(pin.size),
    });
    let expected = Expected {
        digest: pin.sha512,
        size: Some(pin.size),
        cap: MAX_ARCHIVE_BYTES,
    };
    download::fetch::<Sha512>(
        &url,
        &archive,
        &expected,
        download::github_releases(),
        |received, total| {
            on_progress(Progress {
                stage: Stage::Downloading,
                received,
                total,
            });
        },
    )
    .await
    .context("downloading GE-Proton")?;
    on_progress(Progress {
        stage: Stage::Unpacking,
        received: 0,
        total: None,
    });
    unpack(&archive, &staging, pin.tag).await?;
    // A folder of the tag without `proton` is a copy cut short outside decanter's own staging.
    if let Err(err) = tokio::fs::remove_dir_all(target).await
        && err.kind() != io::ErrorKind::NotFound
    {
        return Err(err).context("removing an incomplete GE-Proton folder");
    }
    tokio::fs::rename(staging.join(pin.tag), target)
        .await
        .context("moving GE-Proton into place")?;
    drop(leftovers);
    drop(lock);
    tracing::info!(path = %target.display(), "GE-Proton installed");
    Ok(())
}

/// Takes the folder's install lock, then removes what installs cut short by a crash left behind.
async fn lock(folder: &Path) -> anyhow::Result<File> {
    let folder = folder.to_owned();
    let locked = tokio::task::spawn_blocking(move || {
        let lock = File::create(folder.join(LOCK))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another decanter process is installing GE-Proton",
                ));
            }
            Err(TryLockError::Error(err)) => return Err(err),
        }
        sweep_stale(&folder);
        Ok(lock)
    })
    .await
    .context("locking the GE-Proton folder")?;
    locked.context("locking the GE-Proton folder")
}

/// Best effort, said in the log. Blocking file I/O.
fn sweep_stale(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name == LOCK || !name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match removed {
            Ok(()) => tracing::info!(path = %path.display(), "interrupted GE-Proton install removed"),
            Err(err) => tracing::warn!(path = %path.display(), %err, "cannot remove an interrupted GE-Proton install"),
        }
    }
}

/// The archive and staging folder of an install that ends early, a cancel dropping its future
/// included, removed on a thread of their own since a staging folder holds over a gigabyte.
struct Leftovers(Vec<PathBuf>);

impl Drop for Leftovers {
    fn drop(&mut self) {
        let paths = std::mem::take(&mut self.0);
        let spawned = std::thread::Builder::new()
            .name("ge-proton-cleanup".to_owned())
            .spawn(move || {
                for path in paths {
                    let removed = if path.is_dir() {
                        std::fs::remove_dir_all(&path)
                    } else {
                        std::fs::remove_file(&path)
                    };
                    if let Err(err) = removed
                        && err.kind() != io::ErrorKind::NotFound
                    {
                        tracing::warn!(path = %path.display(), %err, "cannot remove what a GE-Proton install left");
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::warn!(%err, "cannot start removing what a GE-Proton install left");
        }
    }
}

async fn unpack(archive: &Path, staging: &Path, tag: &str) -> anyhow::Result<()> {
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

async fn remove_other_versions(current: &Path) {
    let Some(folder) = current.parent() else {
        return;
    };
    let Ok(mut entries) = tokio::fs::read_dir(folder).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let stale = path != current && !entry.file_name().to_string_lossy().starts_with('.');
        if stale && let Err(err) = tokio::fs::remove_dir_all(&path).await {
            tracing::warn!(path = %path.display(), %err, "cannot remove an old GE-Proton");
        }
    }
}
