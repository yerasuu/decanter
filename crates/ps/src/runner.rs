//! Getting psport and AnyPS5 onto the machine through the operations every runner shares
//! ([`decanter_core::runner`]): the same functions on every system, each answered by the system's own
//! part under [`crate::os`]. The runners `decanter_runtime` gets answer here as runners this crate
//! cannot get.
//!
//! Cancelling is dropping the future. A preparation installs nothing unless every step before it
//! passed, and is a no-op for what is already there. A runner built from a [`Pin::Commit`] runs code
//! nobody vetted for decanter, so it is built only once that commit was confirmed; on Windows that
//! confirmation names the branch the remote listed the commit on, which only the remote's answer
//! carries, so it is recorded by [`crate::os::windows::winbuild::confirm`] and [`confirm`] only
//! checks it. On Linux the app prepares a port itself, its release bundle and its checkout's commit
//! alike, so a port is only reported there ([`Error::AppJob`]).

use std::io;
use std::path::PathBuf;

use decanter_core::capabilities::{Reason, Support};
use decanter_core::runner::{Installed, Pin, Progress, Runner, Status, Tag};

use crate::os::{self, windows};
use crate::paths::Root;
use crate::port::Port;

/// Why a port could not be prepared. The steps' own errors are kept as they are, so the UI words
/// each as it did before there was one interface.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("decanter-ps prepares only psport and AnyPS5")]
    NotAPort,
    #[error("{0} is not prepared on this system")]
    Unsupported(&'static str),
    #[error("no release {0} carries psport's Windows package")]
    NotOffered(Tag),
    #[error("{0}")]
    Package(windows::package::Error),
    #[error("{0}")]
    Build(windows::winbuild::Error),
    #[error("the port is prepared by the app's own jobs on this system")]
    AppJob,
}

const fn port(runner: Runner) -> Option<Port> {
    match runner {
        Runner::Psport => Some(Port::Bbport),
        Runner::AnyPs5 => Some(Port::AnyPs5),
        Runner::Umu | Runner::Proton | Runner::GeProton | Runner::Raven => None,
    }
}

fn not_a_port() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, Error::NotAPort)
}

#[must_use]
pub const fn support(runner: Runner) -> Support {
    match port(runner) {
        Some(port) => os::ports::support(port),
        None => Support::Unavailable(Reason::NotOnThisSystem),
    }
}

/// What is installed, read from disk only.
pub async fn status(root: &Root, runner: Runner) -> Status {
    match port(runner) {
        Some(port) => os::ports::status(root, port).await,
        None => Status::Unsupported,
    }
}

/// Whether building `pin` was confirmed, which [`prepare`] asks for before it builds a
/// [`Pin::Commit`]; a release, checked against its checksum when it is downloaded, needs none.
///
/// # Errors
///
/// Fails if this system cannot get the runner, or `pin` is a commit nobody confirmed.
pub async fn confirm(root: &Root, runner: Runner, pin: &Pin) -> io::Result<()> {
    match port(runner) {
        Some(port) => os::ports::confirm(root, port, pin).await,
        None => Err(not_a_port()),
    }
}

/// Installs `pin` unless it is there already: a release as published, a commit built here. A
/// build's output goes to `on_line` as it comes, and whole to its log ([`log_path`]).
///
/// # Errors
///
/// Fails if this system cannot get the runner or `pin` that way, or any step fails.
pub async fn prepare(
    root: &Root,
    runner: Runner,
    pin: &Pin,
    on_progress: &mut dyn FnMut(Progress),
    on_line: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    match port(runner) {
        Some(port) => os::ports::prepare(root, port, pin, on_progress, on_line).await,
        None => Err(Error::NotAPort),
    }
}

/// Installs the newest release, leaving the ones installed before for the user to remove, since
/// any of them may be the one chosen to play. On Linux the checkout's git moves the port instead.
///
/// # Errors
///
/// As [`prepare`], and when the releases cannot be listed.
pub async fn update(
    root: &Root,
    runner: Runner,
    on_progress: &mut dyn FnMut(Progress),
    on_line: &mut dyn FnMut(String),
) -> Result<Installed, Error> {
    match port(runner) {
        Some(port) => os::ports::update(root, port, on_progress, on_line).await,
        None => Err(Error::NotAPort),
    }
}

/// Removes `pin`; one that is not there is already removed. The caller keeps the one in use.
///
/// # Errors
///
/// Fails if this system cannot get the runner or the files cannot be removed.
pub async fn remove(root: &Root, runner: Runner, pin: &Pin) -> io::Result<()> {
    match port(runner) {
        Some(port) => os::ports::remove(root, port, pin).await,
        None => Err(not_a_port()),
    }
}

/// The whole output of the last build of `pin`; `None` for a release, which is only downloaded.
#[must_use]
pub fn log_path(root: &Root, runner: Runner, pin: &Pin) -> Option<PathBuf> {
    port(runner).and_then(|port| os::ports::log_path(root, port, pin))
}
