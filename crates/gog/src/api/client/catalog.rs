// SPDX-License-Identifier: GPL-3.0-or-later
//! GOG's public store catalog, `catalog.gog.com`, which lists the fan-made mods GOG sells as
//! separate free products under its `mod` tag ([GOG Mods](https://www.gog.com/en/mods)).

use serde::Deserialize;

use super::http::{Call, Host, Sessionless};
use super::Error;

const MODS: Call = Call {
    name: "mods catalog",
    refused: &[],
    max_body: 4 << 20,
    has_secrets: false,
};

/// A product as the catalog lists it, checked for shape only: what is fit to keep of it is the
/// caller's rule.
#[non_exhaustive]
pub struct CatalogProduct {
    /// Decimal digits when GOG sends a number; a string is passed on as it came.
    pub id: String,
    pub title: String,
    pub store_link: String,
}

/// One page of [`Sessionless::catalog_mods`].
#[non_exhaustive]
pub struct CatalogMods {
    /// How many products GOG listed, the ones of the wrong shape included.
    pub listed: usize,
    pub products: Vec<CatalogProduct>,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    products: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProduct {
    id: RawId,
    title: String,
    store_link: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawId {
    Text(String),
    Number(u64),
}

impl Sessionless {
    /// The first `limit` mods of the catalog, by title.
    ///
    /// # Errors
    ///
    /// [`Error::Malformed`] when the answer is not a catalog page or comes from another host; the
    /// others when GOG cannot be asked.
    #[inline]
    pub async fn catalog_mods(&self, limit: usize) -> Result<CatalogMods, Error> {
        let limit_text = limit.to_string();
        let query = [
            ("tags", "in:mod"),
            // Price and region aren't kept, but GOG's store always sends them and the tag was
            // measured with them.
            ("countryCode", "ES"),
            ("locale", "en-US"),
            ("currencyCode", "EUR"),
            ("order", "asc:title"),
            ("limit", limit_text.as_str()),
        ];
        let page: Page = self
            .get(&MODS, Host::Catalog, "/v1/catalog", &query, None)
            .await?;
        Ok(CatalogMods {
            listed: page.products.len(),
            products: page.products.into_iter().filter_map(product).collect(),
        })
    }
}

/// One product of the wrong shape costs only itself.
fn product(raw: serde_json::Value) -> Option<CatalogProduct> {
    let product: RawProduct = serde_json::from_value(raw).ok()?;
    Some(CatalogProduct {
        id: match product.id {
            RawId::Text(id) => id,
            RawId::Number(id) => id.to_string(),
        },
        title: product.title,
        store_link: product.store_link,
    })
}
