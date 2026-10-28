//! What bbport is launched with, kept in its data directory.

use std::fs;
use std::io;
use std::path::PathBuf;

use super::names::{MEMORY_MODEL_SETTINGS, single_memory_model_blocking};
use crate::paths::Root;
use crate::port::Port;

const TITLES: &str = "titles";
const LAUNCHER_PASSWORD: &str = "online_password";

fn path(root: &Root) -> PathBuf {
    root.bbport_data().join("settings.json")
}

/// The launch settings, turned into environment variables by [`super::Bbport::launch_command`].
/// The JSON shape of bbport's former GTK launcher, so its file imports as is; keys decanter
/// doesn't edit are kept as they are, except 0.3's memory model ones once the checkout's port
/// ignores them.
#[derive(Clone, Default)]
pub struct LauncherSettings(serde_json::Map<String, serde_json::Value>);

impl LauncherSettings {
    /// A missing or unreadable file gives the defaults. File I/O: off the UI thread.
    /// A plaintext shadNet password imported from psport's launcher is dropped and the file
    /// written again at once, so it doesn't wait on disk for the next save.
    pub fn load(root: &Root) -> LauncherSettings {
        let mut settings: Self = fs::read(path(root))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .map(LauncherSettings)
            .unwrap_or_default();
        if settings.0.remove(LAUNCHER_PASSWORD).is_some() {
            match settings.clone().save(root) {
                Ok(()) => tracing::info!("dropped the launcher's plaintext shadNet password from the port settings"),
                Err(err) => {
                    tracing::warn!(%err, "could not drop the launcher's plaintext shadNet password from the port settings");
                }
            }
        }
        settings
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

    pub fn save(mut self, root: &Root) -> std::io::Result<()> {
        if single_memory_model_blocking(&Port::Bbport.checkout(root)) {
            self.0.retain(|key, _| !MEMORY_MODEL_SETTINGS.contains(&key.as_str()));
        }
        // psport's launcher keeps the shadNet password here in the clear; decanter keeps it in the
        // keyring only, so an imported file loses it on the first save.
        self.0.remove(LAUNCHER_PASSWORD);
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

    /// A switch of one game rather than of every game the port runs, under `titles.<TITLE_ID>`.
    pub fn title_flag(&self, title_id: &str, key: &str) -> bool {
        self.0
            .get(TITLES)
            .and_then(|titles| titles.get(title_id))
            .and_then(|title| title.get(key))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// Off removes the key, and a title left with none, so the file keeps only what was turned on.
    pub fn set_title_flag(&mut self, title_id: &str, key: &str, value: bool) {
        self.set_title_value(title_id, key, value.then(|| true.into()));
    }

    /// A non-empty text of one game, under `titles.<TITLE_ID>`.
    pub fn title_text(&self, title_id: &str, key: &str) -> Option<&str> {
        self.0
            .get(TITLES)
            .and_then(|titles| titles.get(title_id))
            .and_then(|title| title.get(key))
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
    }

    /// Empty removes the key, like [`Self::set_title_flag`] off.
    pub fn set_title_text(&mut self, title_id: &str, key: &str, value: &str) {
        self.set_title_value(title_id, key, (!value.is_empty()).then(|| value.into()));
    }

    fn set_title_value(&mut self, title_id: &str, key: &str, value: Option<serde_json::Value>) {
        use serde_json::Value;
        let mut titles = match self.0.remove(TITLES) {
            Some(Value::Object(titles)) => titles,
            _ => serde_json::Map::new(),
        };
        let mut values = match titles.remove(title_id) {
            Some(Value::Object(values)) => values,
            _ => serde_json::Map::new(),
        };
        match value {
            Some(value) => {
                values.insert(key.into(), value);
            }
            None => {
                values.remove(key);
            }
        }
        if !values.is_empty() {
            titles.insert(title_id.into(), values.into());
        }
        if !titles.is_empty() {
            self.0.insert(TITLES.into(), titles.into());
        }
    }
}
