//! What bbport is launched with: each title's own file ([`title`]) and the shared one in the
//! port's data directory.

mod title;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::is_bloodborne_title;
use super::names::{MEMORY_MODEL_SETTINGS, single_memory_model_blocking};
use crate::paths::Root;
use crate::port::Port;

const TITLES: &str = "titles";
const LAUNCHER_PASSWORD: &str = "online_password";
/// A title's switch for the user's system modules dump; the folder is `sys_modules_dir`, shared.
pub const SYS_MODULES: &str = "sys_modules";

fn path(root: &Root) -> PathBuf {
    root.bbport_data().join("settings.json")
}

/// The launch settings, turned into environment variables by [`super::Bbport::launch_command`].
/// The JSON shape of bbport's former GTK launcher, so its file imports as is; keys decanter
/// doesn't edit are kept as they are, except 0.3's memory model ones once the checkout's port
/// ignores them.
#[derive(Clone, Default)]
pub struct LauncherSettings {
    values: serde_json::Map<String, serde_json::Value>,
    /// Read from one title's own file, where that title's switches are top-level keys instead of
    /// entries under `titles.<TITLE_ID>`.
    is_title: bool,
}

impl LauncherSettings {
    /// A missing or unreadable file gives the defaults, an unreadable one said in the log. File
    /// I/O: off the UI thread.
    /// A plaintext shadNet password imported from psport's launcher is dropped and the file
    /// written again at once, so it doesn't wait on disk for the next save.
    pub fn load(root: &Root) -> LauncherSettings {
        let mut settings = match Self::read(root) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::warn!(path = %path(root).display(), %err, "cannot read the port settings, using the defaults");
                return Self::default();
            }
        };
        if settings.values.remove(LAUNCHER_PASSWORD).is_some() {
            match settings.clone().save(root) {
                Ok(()) => tracing::info!("dropped the launcher's plaintext shadNet password from the port settings"),
                Err(err) => {
                    tracing::warn!(%err, "could not drop the launcher's plaintext shadNet password from the port settings");
                }
            }
        }
        settings
    }

    /// One change against the shared file as it is now, for the keys every title shares
    /// ([`Self::is_shared`]); a title's own go through [`Self::edit_title`]. Fails rather than
    /// overwrite a file it can't read or parse.
    pub fn edit(root: &Root, change: impl FnOnce(&mut Self)) -> std::io::Result<()> {
        let mut settings = match Self::read(root) {
            Ok(settings) => settings,
            Err(err) => return Err(err),
        };
        change(&mut settings);
        settings.save(root)
    }

    fn read(root: &Root) -> io::Result<Self> {
        Self::read_at(&path(root), false)
    }

    /// The defaults when there is no file yet. A file that doesn't parse names itself in the
    /// error, since the user may have to find it.
    fn read_at(path: &Path, is_title: bool) -> io::Result<Self> {
        match fs::read(path) {
            Ok(data) => serde_json::from_slice(&data)
                .map(|values| Self { values, is_title })
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {err}", path.display()))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self {
                values: serde_json::Map::new(),
                is_title,
            }),
            Err(err) => Err(err),
        }
    }

    /// # Errors
    /// Writing the shared file.
    pub fn save(self, root: &Root) -> std::io::Result<()> {
        self.write_at(root, &path(root))
    }

    fn write_at(mut self, root: &Root, path: &Path) -> io::Result<()> {
        if single_memory_model_blocking(&Port::Bbport.checkout(root)) {
            self.values
                .retain(|key, _| !MEMORY_MODEL_SETTINGS.contains(&key.as_str()));
        }
        // psport's launcher keeps the shadNet password here in the clear; decanter keeps it in the
        // keyring only, so an imported file loses it on the first save.
        self.values.remove(LAUNCHER_PASSWORD);
        let text = serde_json::to_string_pretty(&self.values).map_err(std::io::Error::other)?;
        crate::files::write_atomic(path, text.as_bytes())
    }

    /// A non-empty string value.
    pub fn text(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
    }

    pub fn flag(&self, key: &str, default: bool) -> bool {
        self.values.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn set_text(&mut self, key: &str, value: &str) {
        self.values.insert(key.into(), value.into());
    }

    pub fn remove(&mut self, key: &str) {
        self.values.remove(key);
    }

    pub fn number(&self, key: &str) -> Option<f64> {
        self.values.get(key).and_then(|v| v.as_f64())
    }

    /// Whole numbers stay JSON integers, as the former GTK launcher wrote them (`fps_limit`).
    pub fn set_number(&mut self, key: &str, value: f64) {
        let value = if value.fract() == 0.0 {
            serde_json::Value::from(value as i64)
        } else {
            value.into()
        };
        self.values.insert(key.into(), value);
    }

    pub fn set_flag(&mut self, key: &str, value: bool) {
        self.values.insert(key.into(), value.into());
    }

    /// Whether `title_id` loads the user's system modules dump when its folder is set. By default
    /// every title but Bloodborne, which keeps the path it is known to work on.
    #[must_use]
    pub fn uses_sys_modules(&self, title_id: &str) -> bool {
        self.flag(SYS_MODULES, !is_bloodborne_title(title_id))
    }

    /// A switch of one game rather than of every game the port runs: a top-level key in the
    /// title's own file, `titles.<TITLE_ID>` in the shared one.
    pub fn title_flag(&self, title_id: &str, key: &str) -> bool {
        self.title_values(title_id)
            .and_then(|title| title.get(key))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }

    /// A switch as it applies to `title_id`. In the title's own file that is its own value; in
    /// the shared one, an older port's or an unusual id's, it is on for every title or for this
    /// one, as it was before each title had a file.
    #[must_use]
    pub fn switch_for(&self, title_id: &str, key: &str) -> bool {
        self.title_flag(title_id, key) || (!self.is_title && self.flag(key, false))
    }

    /// Off removes the key, and a title left with none, so the file keeps only what was turned on.
    pub fn set_title_flag(&mut self, title_id: &str, key: &str, value: bool) {
        self.set_title_value(title_id, key, value.then(|| true.into()));
    }

    /// A non-empty text of one game, under `titles.<TITLE_ID>`.
    pub fn title_text(&self, title_id: &str, key: &str) -> Option<&str> {
        self.title_values(title_id)
            .and_then(|title| title.get(key))
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
    }

    /// Empty removes the key, like [`Self::set_title_flag`] off.
    pub fn set_title_text(&mut self, title_id: &str, key: &str, value: &str) {
        self.set_title_value(title_id, key, (!value.is_empty()).then(|| value.into()));
    }

    fn title_values(&self, title_id: &str) -> Option<&serde_json::Map<String, serde_json::Value>> {
        if self.is_title {
            return Some(&self.values);
        }
        self.values
            .get(TITLES)
            .and_then(|titles| titles.get(title_id))
            .and_then(serde_json::Value::as_object)
    }

    fn set_title_value(&mut self, title_id: &str, key: &str, value: Option<serde_json::Value>) {
        use serde_json::Value;
        if self.is_title {
            match value {
                Some(value) => self.values.insert(key.into(), value),
                None => self.values.remove(key),
            };
            return;
        }
        let mut titles = match self.values.remove(TITLES) {
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
            self.values.insert(TITLES.into(), titles.into());
        }
    }
}
