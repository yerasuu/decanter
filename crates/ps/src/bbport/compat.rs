//! What psport says about each title it knows (`out/psport --list-compat`), read once a build or
//! bundle is in place and shown beside the dump instead of decanter's own guess.
//!
//! Ports from before the query don't answer; that is no report, not an error, and decanter's own
//! status stands.

use decanter_nix::Toolchain;
use serde::Deserialize;

use super::{Bbport, features};

/// What the answer may hold before it's taken for something else: a row is a line or two.
const MAX_ANSWER: usize = 512 * 1024;
/// Rows, as [`MAX_ANSWER`] bounds bytes.
const MAX_TITLES: usize = 2048;
/// A title id such as `CUSA03173`.
const MAX_TITLE_ID: usize = 16;
/// A note, as the UI shows it in a game's detail.
const MAX_NOTE: usize = 1024;

/// How a title runs on this port, as psport spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatState {
    /// Starts, but something known is wrong: the game stays launchable.
    Broken,
    Experimental,
    Playable,
    /// Doesn't run: nothing to launch.
    Unsupported,
}

/// One row of the answer, with only the fields decanter reads.
#[derive(Debug, Clone, Deserialize)]
pub struct CompatTitle {
    /// Why the title is in this state; empty when psport gave none.
    #[serde(default)]
    pub note: String,
    /// How it runs on this port.
    pub state: CompatState,
    /// The dump's title id, `CUSA` plus digits.
    pub title_id: String,
}

/// The answer as psport prints it.
#[derive(Deserialize)]
struct Answer {
    /// Every title psport has a state for.
    titles: Vec<CompatTitle>,
}

/// psport's answer, checked: every row with a plain title id and a bounded note.
#[derive(Debug, Clone, Default)]
pub struct Compat {
    titles: Vec<CompatTitle>,
}

impl Compat {
    #[inline]
    #[must_use]
    pub fn get(&self, title_id: &str) -> Option<&CompatTitle> {
        self.titles.iter().find(|title| title.title_id == title_id)
    }

    /// The edge where psport's output becomes decanter's data.
    fn parse(stdout: &[u8]) -> Result<Self, String> {
        if stdout.len() > MAX_ANSWER {
            return Err(format!("answer larger than {MAX_ANSWER} bytes"));
        }
        let answer: Answer = serde_json::from_slice(stdout).map_err(|err| err.to_string())?;
        if answer.titles.len() > MAX_TITLES {
            return Err(format!("more than {MAX_TITLES} titles"));
        }
        if let Some(bad) = answer.titles.iter().find(|title| {
            title.title_id.is_empty()
                || title.title_id.len() > MAX_TITLE_ID
                || !title.title_id.chars().all(|ch| ch.is_ascii_alphanumeric())
                || title.note.len() > MAX_NOTE
        }) {
            return Err(format!("malformed title row {:?}", bad.title_id));
        }
        Ok(Self { titles: answer.titles })
    }
}

/// `None` when the port doesn't answer: not built yet, or a release from before `--list-compat`.
/// An answer that doesn't check out is logged and dropped the same way.
#[inline]
pub async fn compat_table(bbport: &Bbport, toolchain: &Toolchain) -> Option<Compat> {
    let stdout = features::query(bbport, toolchain, "--list-compat", MAX_ANSWER).await?;
    match Compat::parse(&stdout) {
        Ok(compat) => {
            tracing::info!(count = compat.titles.len(), "psport listed its compatibility table");
            Some(compat)
        }
        Err(err) => {
            tracing::warn!(%err, "psport's compatibility list doesn't check out, ignoring it");
            None
        }
    }
}
