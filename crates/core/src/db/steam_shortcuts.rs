//! The Steam shortcut appid assigned to each exported game.

use rusqlite::{OptionalExtension as _, params};

use super::{Db, Error, Result};
use crate::catalog::Storefront;

impl Db {
    /// The Steam `shortcuts.vdf` appid decanter assigned this game, if it's
    /// ever been exported — the removal path's only way to find the right
    /// entry and grid artwork without re-deriving anything from the
    /// game's (possibly since-changed) title.
    pub fn steam_shortcut_appid(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
    ) -> Result<Option<i32>> {
        let appid = self
            .conn
            .query_row(
                "SELECT appid FROM steam_shortcuts WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
                params![account_id, storefront as i32, product_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)?;
        tracing::debug!(
            account_id,
            ?storefront,
            product_id,
            found = appid.is_some(),
            "read steam shortcut appid"
        );
        Ok(appid)
    }

    pub fn set_steam_shortcut_appid(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
        appid: i32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO steam_shortcuts (account_id, storefront, product_id, appid) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(account_id, storefront, product_id) DO UPDATE SET appid = excluded.appid",
            params![account_id, storefront as i32, product_id, appid],
        )?;
        tracing::debug!(account_id, ?storefront, product_id, appid, "set steam shortcut appid");
        Ok(())
    }

    pub fn remove_steam_shortcut_appid(
        &self,
        account_id: &str,
        storefront: Storefront,
        product_id: &str,
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM steam_shortcuts WHERE account_id = ?1 AND storefront = ?2 AND product_id = ?3",
            params![account_id, storefront as i32, product_id],
        )?;
        tracing::debug!(account_id, ?storefront, product_id, "removed steam shortcut appid");
        Ok(())
    }
}
