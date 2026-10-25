//! What a report says about this build and this system, taken from what decanter already knows.

use std::time::{SystemTime, UNIX_EPOCH};

use decanter_report::Host;
use decanter_report::schema::Os;

pub fn host() -> Host {
    let (version, commit) = decanter_core::BUILD
        .split_once(" (")
        .map_or((decanter_core::BUILD, None), |(version, commit)| {
            (version, commit.strip_suffix(')'))
        });
    Host {
        os: if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Macos
        } else {
            Os::Linux
        },
        os_version: os_version(),
        gpu: None,
        decanter_version: version.to_owned(),
        decanter_commit: commit.map(str::to_owned),
        now: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    }
}

/// `SteamOS 3.8.1` from `os-release`; a name or release the report's vocabulary does not know is
/// left out by the builder, so nothing is filtered here.
#[cfg(target_os = "linux")]
fn os_version() -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release")
        .inspect_err(|err| tracing::debug!(%err, "no os-release to read"))
        .ok()?;
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|value| value.trim().trim_matches('"'))
    };
    Some(format!("{} {}", value("NAME")?, value("VERSION_ID")?))
}

/// Windows and macOS name their release through system calls decanter does not make yet.
#[cfg(not(target_os = "linux"))]
const fn os_version() -> Option<String> {
    None
}
