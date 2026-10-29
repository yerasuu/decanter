// SPDX-License-Identifier: GPL-3.0-or-later
//! The account's games as the website's library page lists them, behind
//! `embed.gog.com/account/getFilteredProducts`
//! ([gogapidocs: Listing](https://gogapidocs.readthedocs.io/en/latest/listing.html)).

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

/// An owned product as the listing describes it; only what decanter reads, so a product missing
/// some other field still parses.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProductDetails {
    pub id: i64,
    pub title: String,
    /// Protocol-relative and without size suffix or extension (`//images-2.gog.com/<hash>`): the
    /// caller picks the size.
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
    /// 0 when GOG leaves it out.
    #[serde(default)]
    pub total_pages: u32,
}

impl Client {
    /// One page of the account's games, the most recently bought first; paging is the caller's.
    ///
    /// # Errors
    ///
    /// As [`Client::user`].
    #[inline]
    pub async fn filtered_products(&self, page: NonZeroU32) -> Result<ProductsPage, Error> {
        let page_number = page.to_string();
        // Neither value is documented: `mediaType=1` selects games and `date_purchased` sorts
        // newest first, as GOG's own library page asks.
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
            Ok::<ProductsPage, Error>,
        )
        .await
    }
}
