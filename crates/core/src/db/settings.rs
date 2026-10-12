//! Global key/value settings and the typed accessors built on them.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};

impl Db {
    /// Whether closing the main window should hide it behind a tray icon
    /// instead of quitting — read once at launch (see `ui`'s `main.rs`),
    /// so this is a single global setting, not per-account.
    pub fn close_to_tray(&self) -> Result<bool> {
        let close_to_tray = self.setting("close_to_tray")?.as_deref() == Some("1");
        tracing::debug!(close_to_tray, "read close_to_tray setting");
        Ok(close_to_tray)
    }

    pub fn set_close_to_tray(&self, close_to_tray: bool) -> Result<()> {
        self.set_setting("close_to_tray", if close_to_tray { "1" } else { "0" })
    }

    /// Whether an install/uninstall should also write a real Steam
    /// `shortcuts.vdf` entry for the game — on by default, since that's
    /// the whole point of the feature, but this writes into the actual
    /// Steam user's own config files, so it stays easy to turn off.
    pub fn steam_export_enabled(&self) -> Result<bool> {
        let enabled = self.setting("steam_export_enabled")?.as_deref() != Some("0");
        tracing::debug!(enabled, "read steam_export_enabled setting");
        Ok(enabled)
    }

    pub fn set_steam_export_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting("steam_export_enabled", if enabled { "1" } else { "0" })
    }

    /// Whether a held controller chord or the global shortcut closes the
    /// game decanter started — on by default, since nothing else gets a
    /// player out of a fullscreen game without a mouse.
    pub fn close_game_shortcut_enabled(&self) -> Result<bool> {
        let enabled = self.setting("close_game_shortcut")?.as_deref() != Some("0");
        tracing::debug!(enabled, "read close_game_shortcut setting");
        Ok(enabled)
    }

    pub fn set_close_game_shortcut_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting("close_game_shortcut", if enabled { "1" } else { "0" })
    }

    /// The Proton build to launch with when a game has no per-game choice
    /// of its own — a bare name from `decanter_runtime::steam_runners`, or
    /// `None` for `decanter_runtime::Proton::Latest`.
    pub fn default_proton(&self) -> Result<Option<String>> {
        let proton = self.setting("default_proton")?;
        tracing::debug!(proton = proton.as_deref(), "read default_proton setting");
        Ok(proton)
    }

    pub fn set_default_proton(&self, proton: Option<&str>) -> Result<()> {
        match proton {
            Some(name) => self.set_setting("default_proton", name),
            None => self.delete_setting("default_proton"),
        }
    }

    /// Whether this library's psport follows every master commit, built
    /// locally, rather than the tagged releases its CI publishes prebuilt —
    /// off by default, since a release is the build upstream vouches for.
    pub fn psport_testing(&self) -> Result<bool> {
        let testing = self.setting("psport_channel")?.as_deref() == Some("testing");
        tracing::debug!(testing, "read psport_channel setting");
        Ok(testing)
    }

    pub fn set_psport_testing(&self, testing: bool) -> Result<()> {
        self.set_setting("psport_channel", if testing { "testing" } else { "release" })
    }

    /// The UI language's code (`es`, `en`), or `None` to follow the system
    /// locale.
    pub fn language(&self) -> Result<Option<String>> {
        let language = self.setting("language")?;
        tracing::debug!(language = language.as_deref(), "read language setting");
        Ok(language)
    }

    pub fn set_language(&self, language: Option<&str>) -> Result<()> {
        match language {
            Some(code) => self.set_setting("language", code),
            None => self.delete_setting("language"),
        }
    }

    /// Arbitrary key/value storage — one-off flags and view state that
    /// don't warrant a table of their own (see [`crate::adopt`] and
    /// [`crate::profiles`] for two callers).
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        // Called from several other Db methods on essentially every
        // operation (active profile, close_to_tray, default_proton, ...),
        // so this alone would flood debug level — trace instead.
        let value = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Error::from)?;
        tracing::trace!(key, found = value.is_some(), "read setting");
        Ok(value)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        tracing::debug!(key, value, "set setting");
        Ok(())
    }

    /// Reverts a setting to unset, distinct from setting it to an empty
    /// string — needed wherever `None` means "use the built-in default"
    /// rather than "the empty string".
    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        tracing::debug!(key, "deleted setting");
        Ok(())
    }
}
