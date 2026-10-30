//! The language a storefront game is installed in, chosen per game: kept
//! here rather than in its manifest so the choice outlives an uninstall and
//! the next install starts from it.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};
use crate::catalog::Storefront;

impl Db {
    /// The storefront's own language tag (`es-ES`, `zh-Hans`), or `None` when
    /// the game follows the app's language.
    pub fn game_language(&self, account_id: &str, storefront: Storefront, product_id: &str) -> Result<Option<String>> {
        let language = self
            .conn
            .query_row(
                "SELECT language FROM game_language WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Error::from)?;
        tracing::trace!(account_id, ?storefront, product_id, ?language, "read game language");
        Ok(language)
    }

    pub fn set_game_language(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        language: Option<&str>,
    ) -> Result<()> {
        match language {
            Some(language) => self.conn.execute(
                "INSERT INTO game_language (account_id, storefront, product_id, language) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(account_id, storefront, product_id) DO UPDATE SET language = excluded.language",
                params![account_id, storefront as i32, product_id, language],
            )?,
            None => self.conn.execute(
                "DELETE FROM game_language WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
            )?,
        };
        tracing::info!(account_id, ?storefront, product_id, language, "game language changed");
        Ok(())
    }
}
