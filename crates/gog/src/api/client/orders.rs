// SPDX-License-Identifier: GPL-3.0-or-later
//! The account's order history, behind `embed.gog.com/account/settings/orders/data`: the same
//! endpoint lgogdownloader has used for years, but it backs a web UI, not a contract, so every
//! field is read loosely.

use core::num::NonZeroU32;

use serde::Deserialize;

use super::http::{Call, Host, Sessionless, SESSION_REFUSED};
use super::Error;

/// Orders carry the buyer's details, so a parse error must not quote the body.
const ORDERS: Call = Call {
    name: "order history",
    refused: SESSION_REFUSED,
    max_body: 8 << 20,
    has_secrets: true,
};

/// One page of [`Sessionless::orders_page`].
#[non_exhaustive]
pub struct OrdersPage {
    pub orders: Vec<Order>,
    /// 0 when GOG leaves it out, as it does on the last page of some answers.
    pub total_pages: i64,
}

/// An order that granted something.
#[non_exhaustive]
pub struct Order {
    /// Unix seconds; `None` when GOG sent none or something unreadable.
    pub date: Option<i64>,
    /// The ids GOG sent in a readable shape.
    pub product_ids: Vec<i64>,
}

#[derive(Deserialize)]
struct RawPage {
    #[serde(default)]
    orders: Vec<RawOrder>,
    #[serde(rename = "totalPages", default)]
    total_pages: i64,
}

/// GOG has been seen sending the date and the ids as numbers and as strings.
#[derive(Deserialize)]
struct RawOrder {
    #[serde(default)]
    date: Option<serde_json::Value>,
    #[serde(default)]
    products: Vec<RawProduct>,
}

#[derive(Deserialize)]
struct RawProduct {
    #[serde(default)]
    id: Option<serde_json::Value>,
}

impl Sessionless {
    /// One page of the orders that granted something: cancelled ones would otherwise date a
    /// game the account no longer owns.
    ///
    /// # Errors
    ///
    /// [`Error::Rejected`] when GOG refuses `access_token`, which this client does not renew;
    /// the others when GOG cannot be asked or answers something unreadable.
    #[inline]
    pub async fn orders_page(
        &self,
        access_token: &str,
        page: NonZeroU32,
    ) -> Result<OrdersPage, Error> {
        let page_number = page.to_string();
        let query = [
            ("canceled", "0"),
            ("completed", "1"),
            ("in_progress", "1"),
            ("not_redeemed", "1"),
            ("pending", "0"),
            ("redeemed", "1"),
            ("page", page_number.as_str()),
        ];
        let raw: RawPage = self
            .get(
                &ORDERS,
                Host::Embed,
                "/account/settings/orders/data",
                &query,
                Some(access_token),
            )
            .await?;
        Ok(OrdersPage {
            orders: raw
                .orders
                .into_iter()
                .map(|order| Order {
                    date: order.date.as_ref().and_then(as_i64),
                    product_ids: order
                        .products
                        .iter()
                        .filter_map(|product| product.id.as_ref().and_then(as_i64))
                        .collect(),
                })
                .collect(),
            total_pages: raw.total_pages,
        })
    }
}

fn as_i64(value: &serde_json::Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str()?.parse().ok())
}
