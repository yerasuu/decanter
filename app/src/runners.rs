//! The one way the app gets at runners: each operation a `match` over [`Runner`] onto the crate that
//! owns the runner, `decanter_runtime::runner` for umu-run, Proton, GE-Proton and Raven and
//! `decanter_ps::runner` for psport and AnyPS5.

use decanter_core::runner::Runner;

use crate::releases::{self, Release, ReleaseError};

/// The newest release of a runner decanter follows upstream's releases of; `None` also for one it
/// only ever gets at its own pin (GE-Proton, Raven) or through its channel (psport, AnyPS5).
pub async fn latest(runner: Runner) -> Result<Option<Release>, ReleaseError> {
    match runner {
        Runner::Umu => releases::latest(decanter_runtime::UMU_LAUNCHER_REPO).await,
        Runner::Proton => releases::latest(decanter_runtime::UMU_PROTON_REPO).await,
        Runner::GeProton | Runner::Raven | Runner::Psport | Runner::AnyPs5 => Ok(None),
    }
}
