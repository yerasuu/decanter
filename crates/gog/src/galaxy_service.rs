//! The stand-in `GalaxyCommunication` Windows service (`decanter-galaxy-service`,
//! see that crate's own doc), registered in the prefix of a game that uses the
//! Galaxy SDK so that SDK builds that look for or start the service find one.

use std::path::Path;

use decanter_core::capabilities::Feature;
use decanter_core::platform;
use decanter_runtime::{Prefix, Proton, Service};

use crate::hook::REAL_DLL_BACKUP;

/// Cross-compiled from `crates/galaxy-service` by this crate's `build.rs`
/// and embedded for the same reason as the hook DLL, and gated the same way.
#[cfg(galaxy_hook)]
static GALAXY_COMMUNICATION_EXE: Option<&[u8]> = Some(include_bytes!(env!("GALAXY_SERVICE_EXE_PATH")));
#[cfg(not(galaxy_hook))]
static GALAXY_COMMUNICATION_EXE: Option<&[u8]> = None;

const SERVICE_NAME: &str = "GalaxyCommunication";

/// What a game ships when it links the Galaxy SDK, and what the hook leaves
/// behind once it has swapped its proxy in for it.
const SDK_DLLS: [&str; 3] = ["Galaxy64.dll", "Galaxy.dll", REAL_DLL_BACKUP];

/// Registers the service in `prefix` if the game at `executable` (in
/// `install_dir`) ships a Galaxy SDK DLL there and the prefix doesn't have
/// it yet; run before the game starts.
///
/// Never fails a launch: a game whose SDK doesn't find the service may
/// still run, so a failure is only logged and the next launch tries again.
pub async fn ensure(install_dir: &Path, executable: &Path, prefix: &Prefix, proton: &Proton) {
    let (Some(binary), true) = (
        GALAXY_COMMUNICATION_EXE,
        platform::support(Feature::GalaxyHook).is_offered(),
    ) else {
        return;
    };
    if !ships_sdk(install_dir, executable).await {
        return;
    }
    match prefix.ensure_service(proton, Service::new(SERVICE_NAME, binary)).await {
        Ok(true) => tracing::info!(prefix = %prefix.path().display(), "registered the GalaxyCommunication service"),
        Ok(false) => {}
        Err(err) => {
            tracing::warn!(prefix = %prefix.path().display(), error = %format_args!("{err:#}"), "couldn't register the GalaxyCommunication service, starting without it");
        }
    }
}

/// Looks only beside the executable and in the install's top folder, where
/// GOG games keep the SDK; a scan of the whole install on every launch
/// would cost more than a missed service does.
async fn ships_sdk(install_dir: &Path, executable: &Path) -> bool {
    for dir in [executable.parent(), Some(install_dir)].into_iter().flatten() {
        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            if SDK_DLLS.iter().any(|dll| name.eq_ignore_ascii_case(dll)) {
                return true;
            }
        }
    }
    false
}
