//! Purchase dates, from GOG's order history.
//!
//! This is the one piece of library metadata GOG doesn't expose through
//! any documented API: `/user/data/games` gives ids, `getFilteredProducts`
//! gives catalog data, and neither says when you bought anything. The
//! account pages behind `embed.gog.com/account/settings/orders/data` do,
//! and it's the same endpoint lgogdownloader has used for years — but it
//! backs a web UI, not a contract, so everything here is parsed
//! defensively and a failure degrades to "no date" rather than to an
//! error the caller has to handle.

use std::collections::HashMap;

use anyhow::Context;
use serde::Deserialize;

const ORDERS_URL: &str = "https://embed.gog.com/account/settings/orders/data";

/// Only orders that actually granted something. Cancelled ones would
/// otherwise date a game you no longer own.
const ORDER_FILTERS: &str = "canceled=0&completed=1&in_progress=1&not_redeemed=1&pending=0&redeemed=1";

#[derive(Debug, Deserialize)]
struct OrdersPage {
    #[serde(default)]
    orders: Vec<Order>,
    /// Absent on the last page in some responses, hence the default.
    #[serde(rename = "totalPages", default)]
    total_pages: i64,
}

#[derive(Debug, Deserialize)]
struct Order {
    /// Unix seconds. GOG has been seen sending this as a number and as a
    /// string, so it's taken loosely and coerced below.
    #[serde(default)]
    date: Option<serde_json::Value>,
    #[serde(default)]
    products: Vec<OrderProduct>,
}

#[derive(Debug, Deserialize)]
struct OrderProduct {
    /// Likewise numeric or string depending on the endpoint's mood.
    #[serde(default)]
    id: Option<serde_json::Value>,
}

fn as_i64(value: &serde_json::Value) -> Option<i64> {
    value.as_i64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

/// When each owned product was purchased, keyed by product id.
///
/// A product bought more than once (a bundle re-grant, a gift) keeps its
/// **earliest** order — that's when it entered the library.
pub async fn purchase_dates(account_id: &str) -> anyhow::Result<HashMap<String, i64>> {
    let access_token = crate::achv_try!(
        crate::with_session(account_id, |client| {
            crate::achv_try!(
                client
                    .get_user_data()
                    .map_err(|err| anyhow::anyhow!("GOG session is not valid: {err}"))
            );
            Ok(client.token.borrow().access_token.clone())
        })
        .await
    );

    let http = crate::http_client();
    let mut dates: HashMap<String, i64> = HashMap::new();
    let mut page = 1;

    loop {
        let url = format!("{ORDERS_URL}?{ORDER_FILTERS}&page={page}");
        let response = crate::achv_try!(http.get(&url).bearer_auth(&access_token).send().await);
        let response = crate::achv_try!(
            response
                .error_for_status()
                .context("GOG order history request rejected")
        );
        let body = crate::achv_try!(response.text().await);

        let parsed: OrdersPage = crate::achv_try!(serde_json::from_str(&body).with_context(|| {
            format!(
                "GOG order history had an unexpected shape; first bytes: {}",
                body.chars().take(300).collect::<String>()
            )
        }));
        tracing::debug!(
            page,
            total_pages = parsed.total_pages,
            order_count = parsed.orders.len(),
            "fetched GOG order-history page"
        );

        for order in &parsed.orders {
            let Some(when) = order.date.as_ref().and_then(as_i64) else {
                continue;
            };
            for product in &order.products {
                let Some(id) = product.id.as_ref().and_then(as_i64) else {
                    continue;
                };
                dates
                    .entry(id.to_string())
                    .and_modify(|existing| *existing = (*existing).min(when))
                    .or_insert(when);
            }
        }

        if page >= parsed.total_pages.max(1) {
            break;
        }
        page += 1;
    }

    tracing::debug!(account_id, product_count = dates.len(), "resolved GOG purchase dates");
    Ok(dates)
}
