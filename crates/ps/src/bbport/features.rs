//! What psport says it can do on this host (`out/psport --list-features`,
//! its capability table, decision #540), read once a build or bundle is in
//! place and taken into decanter's own capabilities (decision #538).
//!
//! Ports from before the table don't answer; that is no report, not an
//! error, and decanter's own table stands.

use decanter_core::capabilities::{Capabilities, Feature, Source};
use serde::Deserialize;
use std::env::consts::OS;

use super::{Bbport, Names};
use crate::port::Port;
use crate::toolchain::Toolchain;

/// What the answer may hold before it's taken for something else: psport's
/// table has a dozen or two rows of a sentence each.
const MAX_ANSWER: usize = 256 * 1024;
/// Rows, as [`MAX_ANSWER`] bounds bytes.
const MAX_FEATURES: usize = 256;
/// An id or an area: one `snake_case` word.
const MAX_ID: usize = 64;
/// A reason, as the UI shows it beside a feature.
const MAX_REASON: usize = 512;
/// The area whose features only online play needs: a game runs without it.
const ONLINE_AREA: &str = "net";

/// A feature's state on this host, as psport spells it.
#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum State {
    /// Works here, still being tested.
    Experimental,
    /// Works here.
    Implemented,
    /// Not here, for the row's reason.
    Unavailable,
}

/// One row of the answer, with only the fields decanter reads.
#[derive(Deserialize)]
struct Row {
    /// The platform layer group it belongs to.
    area: String,
    /// The feature's id in psport's table.
    id: String,
    /// How it works here, or why it doesn't.
    reason: String,
    /// Its state on this host.
    state: State,
}

/// The answer as psport prints it.
#[derive(Deserialize)]
struct Answer {
    /// Every feature of the table.
    features: Vec<Row>,
    /// The OS psport was built for: `linux`, `win32` or `macos`.
    os: String,
}

/// psport's answer, checked: every row with a plain id and a bounded
/// reason, for the OS this decanter runs on.
pub struct HostFeatures {
    /// The answer's rows, all of them checked.
    rows: Vec<Row>,
}

impl HostFeatures {
    /// Playing goes when a feature a game needs is unavailable here, with
    /// psport's own reason for the first one, and is marked as being tested
    /// when one is only experimental. Online play has no feature of its own
    /// in decanter yet, so its rows change nothing.
    #[inline]
    pub fn merge_into(&self, capabilities: &mut Capabilities) {
        let needed = self.rows.iter().filter(|row| row.area != ONLINE_AREA);
        let missing = needed.clone().find(|row| matches!(row.state, State::Unavailable));
        match missing {
            Some(row) => capabilities.merge_reported(
                Source::Psport,
                [(Feature::PsPlay, format!("{}: {}", row.id, row.reason))],
            ),
            None if needed.clone().any(|row| matches!(row.state, State::Experimental)) => {
                capabilities.mark_testing(Feature::PsPlay);
            }
            None => {}
        }
    }

    /// The edge where psport's output becomes decanter's data.
    fn parse(stdout: &[u8]) -> Result<Self, String> {
        if stdout.len() > MAX_ANSWER {
            return Err(format!("answer larger than {MAX_ANSWER} bytes"));
        }
        let answer: Answer = serde_json::from_slice(stdout).map_err(|err| err.to_string())?;
        let this_os = if OS == "windows" { "win32" } else { OS };
        if answer.os != this_os {
            return Err(format!("answer for {:?}, not {this_os}", answer.os));
        }
        if answer.features.len() > MAX_FEATURES {
            return Err(format!("more than {MAX_FEATURES} features"));
        }
        let plain = |text: &str| {
            !text.is_empty()
                && text.len() <= MAX_ID
                && text
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
        };
        if let Some(bad) = answer
            .features
            .iter()
            .find(|row| !plain(&row.id) || !plain(&row.area) || row.reason.len() > MAX_REASON)
        {
            return Err(format!("malformed feature row {:?}", bad.id));
        }
        Ok(Self { rows: answer.features })
    }
}

/// `None` when the port doesn't answer: not built yet, or a release from
/// before `--list-features`. An answer that doesn't check out is logged
/// and dropped the same way.
#[inline]
pub async fn host_features(bbport: &Bbport, toolchain: &Toolchain) -> Option<HostFeatures> {
    let stdout = list_features(bbport, toolchain).await?;
    match HostFeatures::parse(&stdout) {
        Ok(features) => {
            tracing::info!(count = features.rows.len(), "psport listed its features");
            Some(features)
        }
        Err(err) => {
            tracing::warn!(%err, "psport's feature list doesn't check out, ignoring it");
            None
        }
    }
}

/// `out/psport --list-features`'s output, from wherever the port runs from.
async fn list_features(bbport: &Bbport, toolchain: &Toolchain) -> Option<Vec<u8>> {
    let port_dir = bbport.port_dir().await;
    if !matches!(Names::of(&port_dir).await, Names::Psport) {
        return None;
    }
    let script = format!(r#"exec "$DECANTER_PORT_DIR/{}" --list-features"#, Names::Psport.probe());
    let answered = match toolchain.command(&bbport.root, Port::Bbport, &script).await {
        Ok(mut command) => command.env("DECANTER_PORT_DIR", port_dir).output().await,
        Err(err) => Err(err),
    };
    match answered {
        Ok(done) if done.status.success() => Some(done.stdout),
        Ok(done) => {
            tracing::info!(status = %done.status, "psport doesn't list its features, keeping decanter's table");
            None
        }
        Err(err) => {
            tracing::warn!(%err, "cannot ask psport for its features");
            None
        }
    }
}
