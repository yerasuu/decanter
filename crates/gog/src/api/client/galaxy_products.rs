// SPDX-License-Identifier: GPL-3.0-or-later
//! GOG's own product API at `api.gog.com/products`, the one Galaxy reads
//! ([gogapidocs](https://gogapidocs.readthedocs.io/en/latest/galaxy.html), unofficial like the
//! endpoint itself): the fallback for everything GamesDB doesn't index.
//!
//! GamesDB only covers base games. A DLC, a bundle, or a game GOG carries but GamesDB never
//! catalogued comes back either as a 404 or as a record whose catalog fields are empty, which is
//! why a third of a real library ends up with no metadata at all: no release date, sometimes not
//! even a title, just the numeric id. This endpoint answers for those. It is narrower than
//! GamesDB (no genres, no developer, no publisher) but it has the three things that matter for an
//! entry to be usable at all: a name, a date, and what kind of product it is.
//!
//! Undocumented but long-stable (every third-party GOG client uses it), so everything here is
//! parsed loosely and a miss costs metadata rather than the entry.

use serde::{Deserialize, Deserializer};

use super::http::{Call, Host, Sessionless};
use super::Error;

/// How many products one request may ask for. The endpoint takes a list; asking one at a time
/// turned a few hundred entries into a few hundred round trips.
pub const PRODUCT_BATCH: usize = 50;

/// Each product comes with its description and every DLC it has, expanded.
const PRODUCTS: Call = Call {
    name: "products",
    refused: &[],
    max_body: 32 << 20,
    has_secrets: false,
};

#[derive(Deserialize)]
#[non_exhaustive]
pub struct Product {
    pub id: i64,
    #[serde(default)]
    pub title: Option<String>,
    /// GOG's own classification: `game`, `dlc`, or `pack`. More accurate than the `isGame` flag
    /// on the owned-products listing, which reports bundles as non-games.
    #[serde(default)]
    pub game_type: Option<String>,
    /// e.g. `2017-06-02T18:55:00+0200`.
    #[serde(default)]
    pub release_date: Option<String>,
    /// Whether the product has anything to install at all.
    ///
    /// `false` for goodies (artbooks, soundtracks, wallpapers). GOG calls them `dlc` like any
    /// other add-on, but they carry no installer and no depot in the game's build: they're bonus
    /// downloads (PDFs, FLAC), not game content. Offering to install one only ever fails.
    #[serde(default)]
    pub is_installable: Option<bool>,
    #[serde(default)]
    pub images: Images,
    #[serde(default)]
    pub description: Description,
    /// The DLC this product has.
    ///
    /// This is the only place GOG states the relationship in its own product ids. GamesDB has a
    /// `parent_id`, but it's GamesDB's internal id and would need resolving separately.
    #[serde(default)]
    pub expanded_dlcs: Vec<Dlc>,
    /// Which systems the product's Galaxy builds are for, part of the default response.
    #[serde(default)]
    pub content_system_compatibility: Option<ContentSystems>,
}

/// The systems a product has Galaxy builds for; only macOS is read.
#[derive(Default, Deserialize)]
#[non_exhaustive]
pub struct ContentSystems {
    /// A macOS build exists.
    // `default` stays because `deserialize_with` drops the implicit `None` for a missing field
    // ([serde: field attributes](https://serde.rs/field-attrs.html#deserialize_with)).
    #[serde(default, deserialize_with = "bool_or_none")]
    pub osx: Option<bool>,
}

/// One DLC, as listed on the product it belongs to.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct Dlc {
    pub id: i64,
}

#[derive(Default, Deserialize)]
#[non_exhaustive]
pub struct Images {
    /// Wide store art, the closest thing here to a hero image.
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub logo: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Default, Deserialize)]
#[non_exhaustive]
pub struct Description {
    /// The short pitch. HTML, like everything on this endpoint.
    #[serde(default)]
    pub lead: Option<String>,
}

impl Sessionless {
    /// A batch of at most [`PRODUCT_BATCH`] products by id, with their DLC lists.
    ///
    /// The response silently omits ids GOG has since delisted (which is most of what has no
    /// metadata), so match the result back by [`Product::id`] rather than by position: a short
    /// response is normal rather than an error.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for more than [`PRODUCT_BATCH`] ids or one that is not positive;
    /// the others when GOG cannot be asked or answers something unreadable.
    #[inline]
    pub async fn products(&self, ids: &[i64]) -> Result<Vec<Product>, Error> {
        if ids.len() > PRODUCT_BATCH || ids.iter().any(|id| *id <= 0) {
            return Err(Error::InvalidInput {
                what: "product id batch",
            });
        }
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let joined = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let query = [
            ("ids", joined.as_str()),
            ("expand", "description,expanded_dlcs"),
        ];
        self.get(&PRODUCTS, Host::Api, "/products", &query, None)
            .await
    }
}

/// A flag in any other shape reads as unknown, so that one odd field in an undocumented response
/// doesn't cost the whole product.
fn bool_or_none<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(|value| value.as_bool())
}
