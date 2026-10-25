//! A profile's `failures.jsonl`, as `decanter_ps::failures::Log` writes it.
//!
//! Only the fields a report may draw from are read: `game_dir`, `log`, `before` and `after` hold
//! paths and raw output and are never deserialized, so they cannot leak into a report by mistake.

use serde::Deserialize;

use crate::schema::{CallCode, FailureKind};

#[derive(Debug, Deserialize)]
pub struct FailureRecord {
    /// Seconds since the Unix epoch; a report keeps its day only.
    pub time: u64,
    pub title_id: String,
    pub version: String,
    #[serde(default)]
    pub commit: String,
    pub kind: RecordKind,
    /// Raw: a line quoted from the output, normalized only when a report is built.
    pub message: String,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub calls: Option<RecordCalls>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub enum RecordKind {
    Crash,
    Stop,
    Error,
    Exit,
    Missing,
}

impl From<RecordKind> for FailureKind {
    fn from(kind: RecordKind) -> Self {
        match kind {
            RecordKind::Crash => Self::Crash,
            RecordKind::Stop => Self::Stop,
            RecordKind::Error => Self::Error,
            RecordKind::Exit => Self::Exit,
            RecordKind::Missing => Self::Missing,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct RecordCalls {
    #[serde(default)]
    pub unimplemented: Vec<RecordCall>,
    #[serde(default)]
    pub stubs: Vec<RecordCall>,
    #[serde(default)]
    pub failed: Vec<RecordCall>,
    #[serde(default)]
    pub status: Vec<RecordCall>,
}

#[derive(Debug, Deserialize)]
pub struct RecordCall {
    pub count: u64,
    pub name: String,
    #[serde(default)]
    pub error: Option<CallCode>,
}

/// The records of a `failures.jsonl`, oldest first. A damaged line is skipped, as decanter's own
/// reader does, so one bad record does not hide the others.
pub fn parse(text: &str) -> impl Iterator<Item = FailureRecord> + '_ {
    text.lines().filter_map(|line| match serde_json::from_str(line) {
        Ok(record) => Some(record),
        Err(error) => {
            tracing::debug!(target: "decanter_report::sources", %error, "skipped an unreadable failure record");
            None
        }
    })
}
