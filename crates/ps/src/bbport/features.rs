//! What psport says it can do on this host (`out/psport --list-features`,
//! its capability table, decision #540), read once a build or bundle is in
//! place and taken into decanter's own capabilities (decision #538).
//!
//! Ports from before the table don't answer; that is no report, not an
//! error, and decanter's own table stands.

use decanter_core::capabilities::{Capabilities, Feature, Source};
use decanter_nix::Toolchain;
use serde::Deserialize;
use std::env::consts::OS;

use super::{Bbport, Names};
use crate::os::linux::toolchain;
use crate::port::Port;

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
/// psport's row for `key.menu` and `pad.menu`, which a game runs without.
const MENU_BINDING: &str = "port_menu_binding";
/// psport's row for `key_stick_opposite` and `key_stick_hold_ms`, which a game runs without.
const KEY_STICK: &str = "key_stick";
/// psport's row for its online module, without which online play has nothing to load.
const ONLINE_MODULE: &str = "online_module";
/// Areas whose rows say whether one setting or option suits this machine (an upscaler, a CPU's
/// fix): a game runs without any of them, so they only decide what the settings view offers.
const OPTION_AREAS: [&str; 2] = ["upscaler", "cpu"];

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
    /// when one is only experimental. The `net` rows decide online play
    /// alone, which a game runs without. The menu binding, the keyboard stick
    /// settings and online play are offered only when the build lists them, since older ones ignore them.
    #[inline]
    pub fn merge_into(&self, capabilities: &mut Capabilities) {
        self.merge_listed(
            capabilities,
            Feature::PsMenuBinding,
            |row| row.id == MENU_BINDING,
            MENU_BINDING,
        );
        self.merge_listed(capabilities, Feature::PsKeyStick, |row| row.id == KEY_STICK, KEY_STICK);
        self.merge_listed(
            capabilities,
            Feature::PsOnline,
            |row| row.area == ONLINE_AREA,
            ONLINE_MODULE,
        );
        let needed = self.rows.iter().filter(|row| {
            row.area != ONLINE_AREA
                && row.id != MENU_BINDING
                && row.id != KEY_STICK
                && !OPTION_AREAS.contains(&row.area.as_str())
        });
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

    /// `feature` from the rows `belongs` picks: gone with the first unavailable one's reason,
    /// marked as being tested when one is experimental, gone as well when `required` isn't listed.
    fn merge_listed(
        &self,
        capabilities: &mut Capabilities,
        feature: Feature,
        belongs: impl Fn(&Row) -> bool,
        required: &str,
    ) {
        if !self.rows.iter().any(|row| row.id == required) {
            capabilities.merge_reported(Source::Psport, [(feature, format!("{required}: not in this build"))]);
            return;
        }
        let mut rows = self.rows.iter().filter(|row| belongs(row));
        match rows.clone().find(|row| matches!(row.state, State::Unavailable)) {
            Some(row) => {
                capabilities.merge_reported(Source::Psport, [(feature, format!("{}: {}", row.id, row.reason))]);
            }
            None if rows.any(|row| matches!(row.state, State::Experimental)) => capabilities.mark_testing(feature),
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

    /// The option rows psport marks unavailable here, by id ([`crate::bbport::OPTION_FEATURES`]):
    /// a build that doesn't list a row leaves its option shown.
    #[inline]
    #[must_use]
    pub fn unavailable_options(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|row| OPTION_AREAS.contains(&row.area.as_str()) && matches!(row.state, State::Unavailable))
            .map(|row| row.id.clone())
            .collect()
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
    let answered = match toolchain::command(toolchain, &bbport.root, Port::Bbport, &script).await {
        Ok(mut command) => {
            command.env("DECANTER_PORT_DIR", port_dir);
            super::query_port(command, MAX_ANSWER).await
        }
        Err(err) => Err(err),
    };
    match answered {
        Ok((status, stdout)) if status.success() => Some(stdout),
        Ok((status, _)) => {
            tracing::info!(%status, "psport doesn't list its features, keeping decanter's table");
            None
        }
        Err(err) => {
            tracing::warn!(%err, "cannot ask psport for its features");
            None
        }
    }
}
