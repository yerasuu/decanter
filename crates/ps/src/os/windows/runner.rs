//! Windows: psport as its release's package ([`package`]) or as its branch built here
//! ([`winbuild`]), both in `<library>/psport/psport-windows/`. AnyPS5 has neither.

use std::io;
use std::path::PathBuf;

use decanter_core::capabilities::{Feature, Reason, Support};
use decanter_core::platform;
use decanter_core::runner::{Commit, Installed, Origin, Pin, Progress, Stage, Status, Tag};

use super::{package, winbuild};
use crate::paths::Root;
use crate::port::Port;
use crate::runner::Error;

fn unsupported(port: Port) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, Error::Unsupported(port.name()))
}

pub const fn support(port: Port) -> Support {
    match port {
        Port::Bbport => platform::support(Feature::PsBuilds),
        Port::AnyPs5 => Support::Unavailable(Reason::NotOnThisSystem),
    }
}

/// The packages, newest first, then the builds, newest first.
pub async fn status(root: &Root, port: Port) -> Status {
    if port == Port::AnyPs5 {
        return Status::Unsupported;
    }
    let packages = package::installed(root).await;
    let builds = winbuild::built(root).await;
    let installed: Vec<Installed> = packages
        .iter()
        .filter_map(|installed| {
            Tag::try_new(installed.version.tag()).map(|tag| Installed {
                pin: Pin::Release(tag),
                origin: Origin::Download,
                path: package::dir(root, &installed.version),
            })
        })
        .chain(builds.into_iter().map(|built| Installed {
            path: winbuild::dir(root, &built.commit),
            pin: Pin::Commit(built.commit),
            origin: Origin::Local,
        }))
        .collect();
    if installed.is_empty() {
        Status::Absent
    } else {
        Status::Installed(installed)
    }
}

pub async fn confirm(root: &Root, port: Port, pin: &Pin) -> io::Result<()> {
    match (port, pin) {
        (Port::AnyPs5, _) => Err(unsupported(port)),
        (Port::Bbport, Pin::Release(_)) => Ok(()),
        (Port::Bbport, Pin::Commit(commit)) => match winbuild::confirmed(root, commit).await {
            Some(_) => Ok(()),
            None => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                winbuild::Error::NotConfirmed(commit.clone()),
            )),
        },
    }
}

pub async fn prepare(
    root: &Root,
    port: Port,
    pin: &Pin,
    on_progress: &mut dyn FnMut(Progress),
    on_line: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    match (port, pin) {
        (Port::AnyPs5, _) => Err(Error::Unsupported(port.name())),
        (Port::Bbport, Pin::Release(tag)) => install(root, tag, on_progress).await,
        (Port::Bbport, Pin::Commit(commit)) => build(root, commit, on_progress, on_line).await,
    }
}

/// The package of `tag`, listed among the releases first since its size is checked as it downloads.
async fn install(root: &Root, tag: &Tag, on_progress: &mut dyn FnMut(Progress)) -> Result<Installed, Error> {
    let Ok(version) = tag.as_str().parse::<package::Version>() else {
        return Err(Error::NotOffered(tag.clone()));
    };
    let installed = Installed {
        pin: Pin::Release(tag.clone()),
        origin: Origin::Download,
        path: package::dir(root, &version),
    };
    if package::installed_version(root, &version).await.is_some() {
        return Ok(installed);
    }
    let offers = match package::offers(Port::Bbport).await {
        Ok(offers) => offers,
        Err(err) => return Err(Error::Package(err)),
    };
    let Some(offer) = offers.iter().find(|offer| offer.version == version) else {
        return Err(Error::NotOffered(tag.clone()));
    };
    let downloaded = package::install(root, Port::Bbport, offer, |received, total| {
        on_progress(Progress {
            stage: Stage::Downloading,
            received,
            total,
            item: None,
        });
    })
    .await;
    match downloaded {
        Ok(_) => Ok(installed),
        Err(err) => Err(Error::Package(err)),
    }
}

/// `commit` built for the branch it was confirmed on, or the build already there.
async fn build(
    root: &Root,
    commit: &Commit,
    on_progress: &mut dyn FnMut(Progress),
    on_line: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    let installed = Installed {
        pin: Pin::Commit(commit.clone()),
        origin: Origin::Local,
        path: winbuild::dir(root, commit),
    };
    // Before the confirmation: a build stays usable after its confirmation dropped off the list.
    if winbuild::built_commit(root, commit).await.is_some() {
        return Ok(installed);
    }
    let Some(head) = winbuild::confirmed(root, commit).await else {
        return Err(Error::Build(winbuild::Error::NotConfirmed(commit.clone())));
    };
    match winbuild::build(root, &head, on_progress, on_line).await {
        Ok(_) => Ok(installed),
        Err(err) => Err(Error::Build(err)),
    }
}

pub async fn update(
    root: &Root,
    port: Port,
    on_progress: &mut dyn FnMut(Progress),
    on_line: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    if port == Port::AnyPs5 {
        return Err(Error::Unsupported(port.name()));
    }
    let newest = match package::offers(Port::Bbport).await {
        Ok(offers) => offers.into_iter().next(),
        Err(err) => return Err(Error::Package(err)),
    };
    let Some(tag) = newest.and_then(|offer| Tag::try_new(offer.version.tag())) else {
        return Err(Error::Package(package::Error::NotPublished));
    };
    prepare(root, port, &Pin::Release(tag), on_progress, on_line).await
}

pub async fn remove(root: &Root, port: Port, pin: &Pin) -> io::Result<()> {
    match (port, pin) {
        (Port::AnyPs5, _) => Err(unsupported(port)),
        (Port::Bbport, Pin::Release(tag)) => match tag.as_str().parse::<package::Version>() {
            Ok(version) => package::remove(root, &version).await,
            // Never a package's folder name, so never installed.
            Err(_) => Ok(()),
        },
        (Port::Bbport, Pin::Commit(commit)) => winbuild::remove(root, commit).await,
    }
}

pub fn log_path(root: &Root, port: Port, pin: &Pin) -> Option<PathBuf> {
    match (port, pin) {
        (Port::Bbport, Pin::Commit(commit)) => Some(winbuild::log_path(root, commit)),
        (Port::Bbport, Pin::Release(_)) | (Port::AnyPs5, _) => None,
    }
}
