//! Which [`GameTool`]s a game starts with: a library-wide default in
//! `settings`, and per game an override that wins over it.
//!
//! Keyed by [`crate::game_key`] rather than by account, so one choice holds
//! for a PC game whichever account launches it, and for a PS title whichever
//! dump of it runs — PS games have no manifest to keep it in.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};
use crate::GameTool;

/// The `settings` key of whether MangoHud logs a run from its start.
const MANGOHUD_AUTOSTART_LOG: &str = "mangohud_autostart_log";

impl Db {
    /// Off unless the user turned it on: the tools change how every game
    /// looks or how the whole machine runs.
    pub fn game_tool_default(&self, tool: GameTool) -> Result<bool> {
        let enabled = self.setting(tool.key())?.as_deref() == Some("1");
        tracing::debug!(tool = tool.key(), enabled, "read game tool default");
        Ok(enabled)
    }

    pub fn set_game_tool_default(&self, tool: GameTool, enabled: bool) -> Result<()> {
        self.set_setting(tool.key(), if enabled { "1" } else { "0" })
    }

    /// `None` when the game follows [`Db::game_tool_default`].
    pub fn game_tool_override(&self, game_key: &str, tool: GameTool) -> Result<Option<bool>> {
        let enabled = self
            .conn
            .query_row(
                "SELECT enabled FROM game_options WHERE game_key = ?1 AND option = ?2",
                params![game_key, tool.key()],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(Error::from)?;
        tracing::trace!(game_key, tool = tool.key(), ?enabled, "read game tool override");
        Ok(enabled)
    }

    pub fn set_game_tool_override(&self, game_key: &str, tool: GameTool, enabled: Option<bool>) -> Result<()> {
        match enabled {
            Some(enabled) => self.conn.execute(
                "INSERT INTO game_options (game_key, option, enabled) VALUES (?1, ?2, ?3)
                 ON CONFLICT(game_key, option) DO UPDATE SET enabled = excluded.enabled",
                params![game_key, tool.key(), enabled],
            )?,
            None => self.conn.execute(
                "DELETE FROM game_options WHERE game_key = ?1 AND option = ?2",
                params![game_key, tool.key()],
            )?,
        };
        tracing::info!(game_key, tool = tool.key(), ?enabled, "game tool override changed");
        Ok(())
    }

    /// Whether `game_key` starts with `tool`: its own choice, else the default.
    pub fn game_tool_enabled(&self, game_key: &str, tool: GameTool) -> Result<bool> {
        self.game_tool_override(game_key, tool)?
            .map_or_else(|| self.game_tool_default(tool), Ok)
    }

    pub fn mangohud_autostart_log(&self) -> Result<bool> {
        Ok(self.setting(MANGOHUD_AUTOSTART_LOG)?.as_deref() == Some("1"))
    }

    pub fn set_mangohud_autostart_log(&self, enabled: bool) -> Result<()> {
        self.set_setting(MANGOHUD_AUTOSTART_LOG, if enabled { "1" } else { "0" })
    }
}
