//! What bbport is launched with, kept in its data directory.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::paths::Root;

fn path(root: &Root) -> PathBuf {
    root.bbport_data().join("settings.json")
}

/// The launch settings, turned into environment variables by [`super::Bbport::launch_command`].
/// The JSON shape of bbport's former GTK launcher, so its file imports as is; keys decanter
/// doesn't edit are kept as they are.
#[derive(Clone, Default)]
pub struct LauncherSettings(serde_json::Map<String, serde_json::Value>);

impl LauncherSettings {
    /// A missing or unreadable file gives the defaults. File I/O: off the UI thread.
    pub fn load(root: &Root) -> LauncherSettings {
        fs::read(path(root))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .map(LauncherSettings)
            .unwrap_or_default()
    }

    /// One change against the file as it is now, for edits made outside
    /// the settings table (folders, extra environment). Fails rather than
    /// overwrite a file it can't read or parse.
    pub fn edit(root: &Root, change: impl FnOnce(&mut Self)) -> std::io::Result<()> {
        let mut settings = match fs::read(path(root)) {
            Ok(data) => match serde_json::from_slice(&data) {
                Ok(map) => Self(map),
                Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidData, err)),
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err),
        };
        change(&mut settings);
        settings.save(root)
    }

    pub fn save(&self, root: &Root) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&self.0).map_err(std::io::Error::other)?;
        crate::files::write_atomic(&path(root), text.as_bytes())
    }

    /// A non-empty string value.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
    }

    pub fn flag(&self, key: &str, default: bool) -> bool {
        self.0.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn set_text(&mut self, key: &str, value: &str) {
        self.0.insert(key.into(), value.into());
    }

    pub fn remove(&mut self, key: &str) {
        self.0.remove(key);
    }

    pub fn number(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(|v| v.as_f64())
    }

    /// Whole numbers stay JSON integers, as the former GTK launcher wrote them (`fps_limit`).
    pub fn set_number(&mut self, key: &str, value: f64) {
        let value = if value.fract() == 0.0 {
            serde_json::Value::from(value as i64)
        } else {
            value.into()
        };
        self.0.insert(key.into(), value);
    }

    pub fn set_flag(&mut self, key: &str, value: bool) {
        self.0.insert(key.into(), value.into());
    }
}
