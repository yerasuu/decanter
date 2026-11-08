// SPDX-License-Identifier: GPL-3.0-or-later
//! What the account owns, installable or not: the entitlement list, read in pages of a thousand
//! (spec, 4.2 and 13.7).
//!
//! The spec reads `entitlementName` and `namespace` from a record. The catalog call needs a
//! catalog item id too, which decanter reads from a `catalogItemId` member: UNVERIFIED that
//! entitlement records carry one (spec, Q3). A record without it is counted and skipped, so an
//! answer without the member leaves the asset list as the only source of titles.

use std::collections::HashSet;

use decanter_core::secrets::Secret;
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;
use url::form_urlencoded::Serializer;

use super::{is_id, is_unreal};
use crate::error::CallError;
use crate::hosts::Host;
use crate::http::{self, Call, Kind};

const ENTITLEMENTS: Call = Call {
    name: "entitlement list",
    host: Host::Entitlements,
    kind: Kind::Bearer,
    max_body: 16 << 20,
};

const PAGE_SIZE: usize = 1000;
/// Fifty thousand entitlements; past this the list says it is incomplete rather than keep asking.
const MAX_PAGES: usize = 50;

/// One owned catalog item.
pub(super) struct Owned {
    pub namespace: String,
    pub catalog_item_id: String,
}

pub(super) struct Entitlements {
    pub owned: Vec<Owned>,
    pub records: usize,
    /// Records without usable ids, or a catalog item already listed.
    pub skipped: usize,
    pub unreal: usize,
    pub private: usize,
    /// Whether the last page was reached. A later page that fails leaves what came before.
    pub complete: bool,
}

#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    namespace: Option<String>,
    #[serde(rename = "catalogItemId", default)]
    catalog_item_id: Option<String>,
    /// The library service marks private sandboxes this way (spec, 4.6 rule 3); UNVERIFIED that an
    /// entitlement record ever carries it, and nothing is dropped where it does not.
    #[serde(rename = "sandboxType", default)]
    sandbox_type: Option<Value>,
}

/// # Errors
///
/// Only when the first page fails: nothing is known then.
pub(super) async fn list(http: &reqwest::Client, token: &Secret, account_id: &str) -> Result<Entitlements, CallError> {
    if !is_id(account_id) {
        return Err(CallError::Malformed {
            call: ENTITLEMENTS.name,
            reason: "the account id is not one decanter can put in a path".to_owned(),
        });
    }
    let mut listed = Entitlements {
        owned: Vec::new(),
        records: 0,
        skipped: 0,
        unreal: 0,
        private: 0,
        complete: false,
    };
    let mut seen = HashSet::new();
    for page in 0..MAX_PAGES {
        let start = page.saturating_mul(PAGE_SIZE);
        let query = Serializer::new(String::new())
            .append_pair("start", &start.to_string())
            .append_pair("count", &PAGE_SIZE.to_string())
            .finish();
        let path = format!("/entitlement/api/account/{account_id}/entitlements?{query}");
        let request = http::request(http, Method::GET, &ENTITLEMENTS, &path).bearer_auth(token.expose());
        let records: Vec<Value> = match http::send(request, &ENTITLEMENTS).await {
            Ok(records) => records,
            Err(err) if page == 0 => return Err(err),
            Err(err) => {
                tracing::warn!(page, error = %err, "an epic entitlement page failed, keeping the earlier pages");
                return Ok(listed);
            }
        };
        let count = records.len();
        listed.records = listed.records.saturating_add(count);
        for raw in records {
            listed.take(raw, &mut seen);
        }
        if count < PAGE_SIZE {
            listed.complete = true;
            return Ok(listed);
        }
    }
    tracing::warn!(
        pages = MAX_PAGES,
        "epic entitlement list still full at the page cap, listing it incomplete"
    );
    Ok(listed)
}

impl Entitlements {
    fn take(&mut self, raw: Value, seen: &mut HashSet<String>) {
        let Ok(record) = serde_json::from_value::<Record>(raw) else {
            self.skipped = self.skipped.saturating_add(1);
            return;
        };
        if record
            .sandbox_type
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|sandbox| sandbox.eq_ignore_ascii_case("PRIVATE"))
        {
            self.private = self.private.saturating_add(1);
            return;
        }
        let (Some(namespace), Some(catalog_item_id)) = (
            record.namespace.filter(|id| is_id(id)),
            record.catalog_item_id.filter(|id| is_id(id)),
        ) else {
            self.skipped = self.skipped.saturating_add(1);
            return;
        };
        if is_unreal(&namespace) {
            self.unreal = self.unreal.saturating_add(1);
        } else if seen.insert(catalog_item_id.clone()) {
            self.owned.push(Owned {
                namespace,
                catalog_item_id,
            });
        } else {
            self.skipped = self.skipped.saturating_add(1);
        }
    }
}
