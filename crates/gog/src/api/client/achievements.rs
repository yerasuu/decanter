// SPDX-License-Identifier: GPL-3.0-only
// Based on gog 0.5.0 (gogapi-rs) by nicohman: the endpoint and the achievement fields come from its
// `Gog::achievements`, `AchievementList` and `Achievement`.
//! The account's achievements in one game, from `gameplay.gog.com`.

use serde::Deserialize;

use super::http::{Call, Client, Host, SESSION_REFUSED};
use super::{auth, Error};

const ACHIEVEMENTS: Call = Call {
    name: "achievements",
    refused: SESSION_REFUSED,
    max_body: 4 << 20,
    has_secrets: false,
};

/// One achievement of a game, unlocked or not.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct Achievement {
    pub achievement_key: String,
    pub name: String,
    pub description: String,
    pub image_url_unlocked: String,
    pub image_url_locked: String,
    /// When the account unlocked it, as GOG writes it; `None` while locked.
    pub date_unlocked: Option<String>,
}

#[derive(Deserialize)]
struct AchievementList {
    items: Vec<Achievement>,
}

impl Client {
    /// Every achievement of `product_id` with this account's progress in it.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a product id that is not positive or a stored session whose
    /// account id is not numeric; otherwise as [`Client::user`].
    #[inline]
    pub async fn achievements(&self, product_id: i64) -> Result<Vec<Achievement>, Error> {
        if product_id <= 0 {
            return Err(Error::InvalidInput { what: "product id" });
        }
        let user_id = self.token().await.user_id;
        if !auth::is_account_id(&user_id) {
            return Err(Error::InvalidInput {
                what: "stored account id",
            });
        }
        let path = format!("/clients/{product_id}/users/{user_id}/achievements");
        self.get(
            &ACHIEVEMENTS,
            Host::Gameplay,
            &path,
            &[],
            |list: AchievementList| Ok(list.items),
        )
        .await
    }
}
