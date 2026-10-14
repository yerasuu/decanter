//! What a run's output taught decanter about a title, so the next run starts the way the port
//! ended up running it. Kept per title in bbport's data directory, beside the launch settings,
//! since it is about the game and not about a profile.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::paths::Root;

/// The end of the line psport's Gnm guard prints once it has caught the game rewriting a frame's
/// command buffers while the GPU still ran them, and turned the overlap off for that session.
const FRAME_OVERLAP_OFF: &str = "frames no longer overlap (BB_SUBMIT_AHEAD 0)";

pub fn is_frame_overlap_off(line: &str) -> bool {
    line.contains(FRAME_OVERLAP_OFF)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FrameOverlap {
    /// Seconds since the Unix epoch, when the guard tripped.
    pub time: u64,
    /// The guard's line, as the port printed it.
    pub line: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Title {
    /// The frames of the session before this record were corrupted until the guard tripped, so
    /// later runs start with `BB_SUBMIT_AHEAD=0` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_overlap_off: Option<FrameOverlap>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Store(BTreeMap<String, Title>);

impl Store {
    fn path(root: &Root) -> PathBuf {
        root.bbport_data().join("learned.json")
    }

    /// A missing or unreadable file gives nothing learned. File I/O: off the UI thread.
    pub fn load(root: &Root) -> Self {
        fs::read(Self::path(root))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn title(&self, title_id: &str) -> Title {
        self.0.get(title_id).cloned().unwrap_or_default()
    }

    /// One change against the file as it is now. Fails rather than overwrite a file it can't read
    /// or parse.
    fn edit(root: &Root, title_id: &str, change: impl FnOnce(&mut Title)) -> io::Result<()> {
        let path = Self::path(root);
        let mut store = match fs::read(&path) {
            Ok(data) => match serde_json::from_slice(&data) {
                Ok(map) => Self(map),
                Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err),
        };
        let title = store.0.entry(title_id.to_owned()).or_default();
        change(title);
        if title.frame_overlap_off.is_none() {
            store.0.remove(title_id);
        }
        let text = serde_json::to_string_pretty(&store.0).map_err(io::Error::other)? + "\n";
        crate::files::write_atomic(&path, text.as_bytes())
    }

    pub fn record_frame_overlap_off(root: &Root, title_id: &str, line: String) -> io::Result<()> {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        Self::edit(root, title_id, |title| {
            title.frame_overlap_off = Some(FrameOverlap { time, line });
        })
    }

    pub fn forget_frame_overlap(root: &Root, title_id: &str) -> io::Result<()> {
        Self::edit(root, title_id, |title| title.frame_overlap_off = None)
    }
}
