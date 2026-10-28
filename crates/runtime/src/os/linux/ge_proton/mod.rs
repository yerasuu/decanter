//! decanter's own GE-Proton: one pinned release, fetched from its `GitHub` release, checked against
//! the pin and unpacked into the library, for what needs a Proton build without Steam (psport's
//! FSR 4.1.1 recording). Games keep the builds umu downloads itself.

mod unpack;

use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use decanter_core::runner::{Installed, Origin, Pin, Progress, Stage, Status, Tag};
use sha2::Sha512;

use crate::download::{self, Expected};
use unpack::unpack;

const REPO: &str = "GloriousEggroll/proton-ge-custom";

/// The one GE-Proton release decanter installs: pinned like `umu-run` and bumped only after
/// psport's FSR 4.1.1 recording ran under it, since psport measured that some Proton builds don't
/// start the DLL's FSR 4.1 at all (GE-Proton 11-6 does, 2026-10-07). `sha512` is the release's own
/// `<asset>.sha512sum` and `size` its `<asset>.tar.gz` asset's bytes (the release publishes one per
/// architecture); an install refuses while either is missing.
const GE_PROTON: GeProtonPin = GeProtonPin {
    tag: "GE-Proton11-6",
    asset: "GE-Proton11-6-x86_64",
    sha512: "543e3af57bb138b1be5a5b98bba4d39ca59340bfa34ec8c12144f3e16d7434ed75bd7a68eafc228b16695884629595af0905156e5227c1898f93cdbc92cb5fcb",
    size: 533_700_853,
};

struct GeProtonPin {
    tag: &'static str,
    /// The archive's name without `.tar.gz`, which is also the one folder inside it.
    asset: &'static str,
    /// Lower-case hex SHA-512 of `<asset>.tar.gz`.
    sha512: &'static str,
    size: u64,
}

/// A GE-Proton archive is about half a gigabyte: the cap only stops a wrong or runaway download.
const MAX_ARCHIVE_BYTES: u64 = 1 << 30;

/// What umu and psport's script read from a Proton folder.
pub(super) const REQUIRED: [&str; 2] = ["proton", "toolmanifest.vdf"];

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
pub(crate) async fn ge_proton_prepare(pin: &Pin, on_progress: &mut dyn FnMut(Progress)) -> anyhow::Result<Installed> {
    let Pin::Release(tag) = pin else {
        bail!("GE-Proton is downloaded from its releases, never built from {pin}");
    };
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
pub(crate) async fn ge_proton_update(on_progress: &mut dyn FnMut(Progress)) -> anyhow::Result<Installed> {
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
    let Pin::Release(tag) = pin else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("GE-Proton has no build of {pin}"),
        ));
    };
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

async fn install(folder: &Path, target: &Path, on_progress: &mut dyn FnMut(Progress)) -> anyhow::Result<()> {
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
        "https://github.com/{REPO}/releases/download/{tag}/{asset}.tar.gz",
        tag = pin.tag,
        asset = pin.asset
    );
    tracing::info!(url, "downloading GE-Proton");
    on_progress(Progress {
        stage: Stage::Downloading,
        received: 0,
        total: Some(pin.size),
        item: None,
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
                item: None,
            });
        },
    )
    .await
    .context("downloading GE-Proton")?;
    on_progress(Progress {
        stage: Stage::Unpacking,
        received: 0,
        total: None,
        item: None,
    });
    unpack(&archive, &staging, pin.asset).await?;
    // A folder of the tag without `proton` is a copy cut short outside decanter's own staging.
    if let Err(err) = tokio::fs::remove_dir_all(target).await
        && err.kind() != io::ErrorKind::NotFound
    {
        return Err(err).context("removing an incomplete GE-Proton folder");
    }
    tokio::fs::rename(staging.join(pin.asset), target)
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
