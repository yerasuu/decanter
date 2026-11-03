// SPDX-License-Identifier: GPL-3.0-or-later
//! One game's achievements with the account's progress, from Galaxy's `gameplay.gog.com`
//! ([gogapidocs: Galaxy](https://gogapidocs.readthedocs.io/en/latest/galaxy.html)).

use serde::{Deserialize, Deserializer};
use serde_json::Value;

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
    /// The numeric id the Galaxy SDK uses; `None` when GOG omits it or sends something that is not a positive integer.
    #[serde(default, deserialize_with = "lenient_id")]
    pub achievement_id: Option<i64>,
    /// GOG's `visible`: whether the achievement shows its details while locked.
    #[serde(default, rename = "visible")]
    pub visible_while_locked: Option<bool>,
    /// Share of players with it, in percent; `None` outside 0 to 100.
    #[serde(default, deserialize_with = "lenient_rarity")]
    pub rarity: Option<f64>,
    #[serde(default, deserialize_with = "lenient_text::<64, _>")]
    pub rarity_level_description: Option<String>,
    #[serde(default, deserialize_with = "lenient_text::<32, _>")]
    pub rarity_level_slug: Option<String>,
    /// The listing's `achievements_mode`, repeated on every item; never part of an item's own JSON.
    #[serde(skip)]
    pub mode: Option<String>,
}

#[derive(Deserialize)]
struct Listing {
    items: Vec<Achievement>,
    #[serde(default, deserialize_with = "lenient_text::<32, _>")]
    achievements_mode: Option<String>,
}

// A malformed optional field is dropped instead of failing the whole listing, so the value is read
// as a generic JSON value first, which always consumes it.
fn lenient_id<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: Deserializer<'de>,
{
    let parsed = match Value::deserialize(deserializer)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.parse().ok(),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => None,
    };
    Ok(parsed.filter(|id| *id > 0))
}

fn lenient_rarity<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let percent = Value::deserialize(deserializer)?.as_f64();
    Ok(percent.filter(|value| (0.0_f64..=100.0_f64).contains(value)))
}

fn lenient_text<'de, const MAX: usize, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = match Value::deserialize(deserializer)? {
        Value::String(text) => Some(text),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => None,
    };
    Ok(value.filter(|text| !text.is_empty() && text.len() <= MAX && !text.chars().any(char::is_control)))
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
            let Listing {
                mut items,
                achievements_mode,
            } = listing;
            for item in &mut items {
                item.mode.clone_from(&achievements_mode);
            }
            Ok(items)
        })
        .await
    }
}
