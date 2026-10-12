//! Favorited games, per account.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};
use crate::catalog::Storefront;

impl Db {
    pub fn is_favorite(&self, account_id: &str, storefront: Storefront, product_id: &str) -> Result<bool> {
        let is_favorite = self
            .conn
            .query_row(
                "SELECT 1 FROM favorites WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            is_favorite,
            "checked favorite status"
        );
        Ok(is_favorite)
    }

    /// Every favorited product id for an account — the sidebar's
    /// "Favoritos" filter checks membership in this set.
    pub fn list_favorite_ids(
        &self,
        account_id: &str,
        storefront: Storefront,
    ) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT product_id FROM favorites WHERE account_id = ?1 AND storefront = ?2")?;
        let rows = stmt.query_map(params![account_id, storefront as i32], |row| row.get(0))?;
        let ids = rows
            .collect::<rusqlite::Result<std::collections::HashSet<_>>>()
            .map_err(Error::from)?;
        tracing::debug!(account_id, ?storefront, count = ids.len(), "listed favorite ids");
        Ok(ids)
    }

    pub fn set_favorite(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        favorite: bool,
    ) -> Result<()> {
        if favorite {
            self.conn.execute(
                "INSERT OR IGNORE INTO favorites (account_id, storefront, product_id) VALUES (?1, ?2, ?3)",
                params![account_id, storefront as i32, product_id],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM favorites WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
            )?;
        }
        tracing::debug!(account_id, ?storefront, product_id, favorite, "set favorite state");
        Ok(())
    }
}
