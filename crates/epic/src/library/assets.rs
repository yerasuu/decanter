// SPDX-License-Identifier: GPL-3.0-or-later
//! The launcher's asset list: the titles the account can install on Windows (spec, 4.1).
//!
//! This is the call the owner saw stall past legendary's 10 s on a valid token while the same
//! route answered an unauthenticated request in 0.14 s (spec, section 12), so it runs on a longer
//! limit of its own, and how long it took is logged whatever it ends in (spec, Q1).

use core::time::Duration;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use decanter_core::secrets::Secret;
use reqwest::Method;
use serde::Deserialize;
use serde_json::Value;

use super::{is_id, is_unreal, printable};
use crate::error::CallError;
use crate::hosts::Host;
use crate::http::{self, Call, Kind};

const PATH: &str = "/launcher/api/public/assets/Windows?label=Live";

const ASSETS: Call = Call {
    name: "asset list",
    host: Host::Launcher,
    kind: Kind::Bearer,
    max_body: 16 << 20,
};

/// Four times the client's 30 s, and the longest wait spec Q1 asks to try: one listing records
/// whether the stall ends inside it, while the call runs beside the others so nothing on screen
/// waits on it, and a stall that never ends still ends here.
pub(crate) const TIMEOUT: Duration = Duration::from_mins(2);

const MAX_ASSETS: usize = 20_000;
const BUILD_VERSION_MAX_CHARS: usize = 128;

/// One installable title. `labelName`, `metadata` and `sidecarRvn` matter only to launches and
/// sidecars, which no stage reads yet.
pub(crate) struct Asset {
    pub app_name: String,
    pub namespace: String,
    pub catalog_item_id: String,
    /// The build's version label, compared with the installed one for updates (spec, 4.1).
    pub build_version: Option<String>,
}

pub(crate) struct Assets {
    pub list: Vec<Asset>,
    /// Entries without three usable ids, repeated, or past the cap.
    pub skipped: usize,
    /// Unreal Engine marketplace content (spec, 4.6 rule 1).
    pub unreal: usize,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "appName", default)]
    app_name: Option<String>,
    #[serde(default)]
    namespace: Option<String>,
    #[serde(rename = "catalogItemId", default)]
    catalog_item_id: Option<String>,
    #[serde(rename = "buildVersion", default)]
    build_version: Option<String>,
}

/// The asset list and how long Epic took to give it, or to fail to.
pub(super) async fn timed(http: reqwest::Client, token: Arc<Secret>) -> (Result<Assets, CallError>, Duration) {
    let started = Instant::now();
    let listed = list(&http, &token).await;
    (listed, started.elapsed())
}

pub(crate) async fn list(http: &reqwest::Client, token: &Secret) -> Result<Assets, CallError> {
    let request = http::request(http, Method::GET, &ASSETS, PATH)
        .bearer_auth(token.expose())
        .timeout(TIMEOUT);
    let entries: Vec<Value> = http::send(request, &ASSETS).await?;
    let mut assets = Assets {
        list: Vec::with_capacity(entries.len().min(MAX_ASSETS)),
        skipped: 0,
        unreal: 0,
    };
    let mut seen = HashSet::new();
    for raw in entries {
        let Some(asset) = asset(raw) else {
            assets.skipped = assets.skipped.saturating_add(1);
            continue;
        };
        if is_unreal(&asset.namespace) {
            assets.unreal = assets.unreal.saturating_add(1);
        } else if assets.list.len() >= MAX_ASSETS || !seen.insert(asset.app_name.clone()) {
            assets.skipped = assets.skipped.saturating_add(1);
        } else {
            assets.list.push(asset);
        }
    }
    Ok(assets)
}

fn asset(raw: Value) -> Option<Asset> {
    let entry: Entry = serde_json::from_value(raw).ok()?;
    let app_name = entry.app_name.filter(|id| is_id(id))?;
    let namespace = entry.namespace.filter(|id| is_id(id))?;
    let catalog_item_id = entry.catalog_item_id.filter(|id| is_id(id))?;
    let build_version = entry
        .build_version
        .and_then(|version| printable(&version, BUILD_VERSION_MAX_CHARS));
    Some(Asset {
        app_name,
        namespace,
        catalog_item_id,
        build_version,
    })
}
