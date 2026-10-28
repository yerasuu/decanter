//! Getting the runners this crate owns onto the machine: the same functions
//! on every system, each answered by the system's own part under `os/`.
//!
//! Cancelling is dropping the future. A preparation installs nothing unless
//! every step before it passed, and is a no-op for what is already there.

use std::io;
use std::path::PathBuf;

use anyhow::bail;

use decanter_core::capabilities::{Feature, Support};
use decanter_core::platform;
use decanter_core::runner::{Installed, Pin, Progress, Runner, Status};

use crate::os;

/// Why Steam's Proton builds are only listed: Steam and umu own `compatibilitytools.d`.
const UMU_INSTALLS_PROTON: &str = "Proton builds are installed by umu at launch or by the user, not by decanter";

#[must_use]
#[inline]
pub const fn support(runner: Runner) -> Support {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton => platform::support(Feature::Proton),
    }
}

/// What is installed, read from disk only.
#[must_use]
#[inline]
pub fn status(runner: Runner) -> Status {
    match runner {
        Runner::Umu => os::umu_status(),
        Runner::Proton => os::proton_status(),
        Runner::GeProton => os::ge_proton_status(),
    }
}

/// Installs `pin` unless it is there already.
///
/// # Errors
///
/// Fails if this system cannot get the runner, `pin` is not one decanter
/// tested it at, or a download or install step fails.
#[inline]
pub async fn prepare(
    runner: Runner,
    pin: &Pin,
    on_progress: &mut (dyn FnMut(Progress) + Send),
) -> anyhow::Result<Installed> {
    match runner {
        Runner::Umu => os::umu_prepare(pin, on_progress).await,
        Runner::Proton => bail!(UMU_INSTALLS_PROTON),
        Runner::GeProton => os::ge_proton_prepare(pin, on_progress).await,
    }
}

/// Brings the runner to the version decanter wants, which for a pinned one
/// is its pin rather than the newest release, and only then removes the
/// versions it replaces.
///
/// # Errors
///
/// As [`prepare`].
#[inline]
pub async fn update(runner: Runner, on_progress: &mut (dyn FnMut(Progress) + Send)) -> anyhow::Result<Installed> {
    match runner {
        Runner::Umu => os::umu_update(on_progress).await,
        Runner::Proton => bail!(UMU_INSTALLS_PROTON),
        Runner::GeProton => os::ge_proton_update(on_progress).await,
    }
}

/// Removes `pin`; one that is not there is already removed.
///
/// # Errors
///
/// Fails if this system cannot get the runner or the files cannot be removed.
#[inline]
pub async fn remove(runner: Runner, pin: &Pin) -> io::Result<()> {
    match runner {
        Runner::Umu => os::umu_remove(pin).await,
        Runner::Proton => Err(io::Error::new(io::ErrorKind::Unsupported, UMU_INSTALLS_PROTON)),
        Runner::GeProton => os::ge_proton_remove(pin).await,
    }
}

/// The log of the last build of `pin`; `None` for a runner that is only
/// ever downloaded.
#[must_use]
#[inline]
pub const fn log_path(runner: Runner, _: &Pin) -> Option<PathBuf> {
    match runner {
        Runner::Umu | Runner::Proton | Runner::GeProton => None,
    }
}
