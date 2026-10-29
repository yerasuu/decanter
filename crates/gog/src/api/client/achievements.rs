// SPDX-License-Identifier: GPL-3.0-or-later
//! One game's achievements with the account's progress, from Galaxy's `gameplay.gog.com`
//! ([gogapidocs: Galaxy](https://gogapidocs.readthedocs.io/en/latest/galaxy.html)).

use serde::Deserialize;

use super::Error;
use super::auth::is_account_id;
use super::http::{Call, Client, Host, SESSION_REFUSED};

const ACHIEVEMENTS: Call = Call {
    name: "achievements",
    refused: SESSION_REFUSED,
    max_body: 4 << 20,
    has_secrets: false,
};

/// An achievement as GOG lists it, with the field names GOG uses.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct Achievement {
    pub achievement_key: String,
    pub name: String,
    pub description: String,
    pub image_url_unlocked: String,
    pub image_url_locked: String,
    /// GOG's own text, such as `2021-10-12T00:00:56+0000`; `None` while locked.
    #[serde(default)]
    pub date_unlocked: Option<String>,
}

#[derive(Deserialize)]
struct Listing {
    items: Vec<Achievement>,
}

impl Client {
    /// Every achievement of `product_id`, unlocked or not. GOG answers the whole list at once,
    /// so no paging is done.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a product id that is not positive or a stored session whose
    /// account id is not numeric, before anything is sent; otherwise as [`Client::user`].
    #[inline]
    pub async fn achievements(&self, product_id: i64) -> Result<Vec<Achievement>, Error> {
        if product_id <= 0 {
            return Err(Error::InvalidInput { what: "product id" });
        }
        // The account id comes from the keyring and becomes a path segment.
        let account_id = self.token().await.user_id;
        if !is_account_id(&account_id) {
            return Err(Error::InvalidInput {
                what: "stored account id",
            });
        }
        let path = format!("/clients/{product_id}/users/{account_id}/achievements");
        self.get(&ACHIEVEMENTS, Host::Gameplay, &path, &[], |listing: Listing| {
            Ok(listing.items)
        })
        .await
    }
}
