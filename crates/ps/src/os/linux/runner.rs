//! Linux: a port as its release's bundle ([`bundle`]), or its checkout at a commit, fetched from
//! the port's nix binary cache ([`super::prebuilt`]) or built in nix. All of it is the app's
//! preparation of the port, one sequence in which a bundle that cannot be used sends the port on to
//! the cache and the build, which run as jobs of the app (decision D4 of #1091: stopping one signals
//! its process group, and its output is that job's log); so here a port is only reported and
//! removed, never prepared.

use std::io;
use std::path::PathBuf;

use decanter_core::capabilities::{Feature, Support};
use decanter_core::platform;
use decanter_core::runner::{Commit, Installed, Origin, Pin, Progress, Status, Tag};

use super::builds::Record;
use super::{bundle, prebuilt};
use crate::bbport::Bbport;
use crate::paths::Root;
use crate::port::Port;
use crate::runner::Error;

pub const fn support(_: Port) -> Support {
    platform::support(Feature::PsBuilds)
}

/// The release bundle the port runs, else the last commit it built or fetched at.
pub async fn status(root: &Root, port: Port) -> Status {
    if port == Port::Bbport {
        let bbport = Bbport::new(root.clone());
        if let Some(tag) = bbport.bundle_tag().await
            && bundle::is_installed(root, port, &tag).await
            && let Some(pin) = Tag::try_new(&tag)
        {
            return Status::Installed(vec![Installed {
                path: bundle::dir(root, port, &tag),
                pin: Pin::Release(pin),
                origin: Origin::Bundle,
            }]);
        }
    }
    let record = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || Record::load(&root, port)).await
    };
    let Some(commit) = record
        .ok()
        .and_then(|record| record.good)
        .as_deref()
        .and_then(Commit::try_new)
    else {
        return Status::Absent;
    };
    let prebuilt = match port {
        Port::Bbport => Bbport::new(root.clone()).prebuilt().await,
        Port::AnyPs5 => None,
    };
    let (origin, path) = match prebuilt {
        Some(store_path) => (Origin::Cache, store_path),
        None => (Origin::Local, port.checkout(root)),
    };
    Status::Installed(vec![Installed {
        pin: Pin::Commit(commit),
        origin,
        path,
    }])
}

/// Nothing to confirm: the checkout follows the port's own upstream.
#[expect(
    clippy::unused_async,
    reason = "the same signature as the systems whose part awaits, so the facade calls each alike"
)]
pub async fn confirm(_: &Root, _: Port, _: &Pin) -> io::Result<()> {
    Ok(())
}

#[expect(
    clippy::unused_async,
    reason = "the same signature as the systems whose part awaits, so the facade calls each alike"
)]
pub async fn prepare(
    _: &Root,
    _: Port,
    _: &Pin,
    _: &mut dyn FnMut(Progress),
    _: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    Err(Error::AppJob)
}

#[expect(
    clippy::unused_async,
    reason = "the same signature as the systems whose part awaits, so the facade calls each alike"
)]
pub async fn update(
    _: &Root,
    _: Port,
    _: &mut dyn FnMut(Progress),
    _: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    Err(Error::AppJob)
}

/// A release's bundle; a commit's build goes with the checkout it was built in.
pub async fn remove(root: &Root, port: Port, pin: &Pin) -> io::Result<()> {
    match pin {
        Pin::Release(tag) if prebuilt::valid_tag(tag.as_str()) => {
            match tokio::fs::remove_dir_all(bundle::dir(root, port, tag.as_str())).await {
                Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
                Ok(()) | Err(_) => Ok(()),
            }
        }
        // Never a bundle's folder name, so never installed.
        Pin::Release(_) => Ok(()),
        Pin::Commit(_) => Err(io::Error::new(io::ErrorKind::Unsupported, Error::AppJob)),
    }
}

/// Nothing here: a build's output is the log of the app's job that ran it.
pub const fn log_path(_: &Root, _: Port, _: &Pin) -> Option<PathBuf> {
    None
}
