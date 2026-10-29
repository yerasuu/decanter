// SPDX-License-Identifier: GPL-3.0-only
// Based on gog 0.5.0 (gogapi-rs) by nicohman: the endpoint and `ProductDetails` come from its
// `Gog::get_filtered_products`; the purchase-order sort from decanter-gog's library listing.
//! The account's products as GOG's own library page lists them.

use core::num::NonZeroU32;

use serde::Deserialize;

use super::Error;
use super::http::{Call, Client, Host, SESSION_REFUSED};

const FILTERED_PRODUCTS: Call = Call {
    name: "filtered products",
    refused: SESSION_REFUSED,
    max_body: 4 << 20,
    has_secrets: false,
};

/// One product of a [`ProductsPage`], with only the fields decanter reads.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProductDetails {
    pub id: i64,
    pub title: String,
    /// GOG's image URL without its size suffix and extension.
    pub image: String,
    pub is_game: bool,
    pub is_movie: bool,
}

/// One page of [`Client::filtered_products`].
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProductsPage {
    #[serde(default)]
    pub products: Vec<ProductDetails>,
    /// How many pages the listing has; 0 when GOG leaves it out.
    #[serde(default)]
    pub total_pages: u32,
}

impl Client {
    /// One page of the owned games (`mediaType=1`), most recently purchased first, so that the
    /// position across pages is the order they joined the library. Products GOG files under no
    /// media type, such as some grants, are only in [`Client::owned_games`].
    ///
    /// # Errors
    ///
    /// As [`Client::user`].
    #[inline]
    pub async fn filtered_products(&self, page: NonZeroU32) -> Result<ProductsPage, Error> {
        let page_number = page.to_string();
        let query = [
            ("mediaType", "1"),
            ("sortBy", "date_purchased"),
            ("page", page_number.as_str()),
        ];
        self.get(
            &FILTERED_PRODUCTS,
            Host::Embed,
            "/account/getFilteredProducts",
            &query,
            Ok,
        )
        .await
    }
}
