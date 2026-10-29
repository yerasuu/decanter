//! Purchase dates, from GOG's order history.
//!
//! This is the one piece of library metadata GOG doesn't expose through
//! any documented API: `/user/data/games` gives ids, `getFilteredProducts`
//! gives catalog data, and neither says when you bought anything. The
//! account pages' order history ([`crate::api::client::Client::orders_page`]) does, so a
//! failure degrades to "no date" rather than to an error the caller has
//! to handle.

use std::collections::HashMap;
use std::num::NonZeroU32;

/// When each owned product was purchased, keyed by product id.
///
/// A product bought more than once (a bundle re-grant, a gift) keeps its
/// **earliest** order — that's when it entered the library.
pub async fn purchase_dates(account_id: &str) -> anyhow::Result<HashMap<String, i64>> {
    let mut dates: HashMap<String, i64> = HashMap::new();
    crate::achv_try!(
        crate::session::with_client(account_id, async |client| {
            let mut page = NonZeroU32::MIN;

            loop {
                let parsed = crate::achv_try!(
                    client
                        .orders_page(page)
                        .await
                        .map_err(|err| crate::session::failed("GOG order history request failed", err))
                );
                tracing::debug!(
                    page,
                    total_pages = parsed.total_pages,
                    order_count = parsed.orders.len(),
                    "fetched GOG order-history page"
                );

                for order in &parsed.orders {
                    let Some(when) = order.date else {
                        continue;
                    };
                    for id in &order.product_ids {
                        dates
                            .entry(id.to_string())
                            .and_modify(|existing| *existing = (*existing).min(when))
                            .or_insert(when);
                    }
                }

                if i64::from(page.get()) >= parsed.total_pages.max(1) {
                    break;
                }
                let Some(next) = page.checked_add(1) else {
                    break;
                };
                page = next;
            }
            Ok(())
        })
        .await
    );

    tracing::debug!(account_id, product_count = dates.len(), "resolved GOG purchase dates");
    Ok(dates)
}
