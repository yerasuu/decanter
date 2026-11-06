// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether Amazon has published a newer version of an installed game: `GetLiveVersionIds` on the
//! distribution service (docs/amazon-nile-integration-analysis.md, section 4.1, step 4). Same host,
//! headers and token as the download request. UNVERIFIED against Amazon until the owner's first
//! check, so the answer is read strictly and anything else is logged with its counts.

use std::collections::HashMap;

use anyhow::Context as _;
use decanter_core::InstalledGame;
use reqwest::StatusCode;
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::CallError;
use crate::http::{self, Call};
use crate::identity::{DISTRIBUTION_PATH, DISTRIBUTION_USER_AGENT, GAMING_HOST, LIVE_VERSIONS_TARGET};
use crate::library::is_id;
use crate::session::{self, Live};

const LIVE_VERSIONS: Call = Call {
    name: "live versions",
    refused: &[StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN],
    max_body: 1 << 20,
};

#[derive(Serialize)]
struct Request<'req> {
    #[serde(rename = "adgProductIds")]
    product_ids: &'req [&'req str],
    #[serde(rename = "Operation")]
    operation: &'static str,
}

/// Entries stay raw so that a version of an unexpected type is counted, not a parse failure of the
/// whole answer.
#[derive(Deserialize)]
struct Answer {
    #[serde(default, rename = "adgProductIdToVersionIdMap")]
    versions: Option<HashMap<String, Value>>,
}

/// `Some(version_id)` when Amazon's live version of `game` differs from the one installed, `None`
/// when it is current.
///
/// # Errors
///
/// A closed session, Amazon not answering, or an answer that does not name a usable version for the
/// game; the reason is fit for the user.
#[inline]
pub async fn check_update(account_id: &str, game: &InstalledGame) -> anyhow::Result<Option<String>> {
    let product_id = game.game_id.as_str();
    anyhow::ensure!(is_id(product_id), "the Amazon product id is not usable");
    let session = match session::live_session(account_id).await? {
        Live::Ready(session) => session,
        Live::LoggedOut | Live::Refused => {
            anyhow::bail!("Amazon closed the session: log in to Amazon Games again in Settings")
        }
        Live::Unreachable(reason) => anyhow::bail!("Amazon could not be asked: {reason}"),
    };
    let http = http::client_for(GAMING_HOST, DISTRIBUTION_USER_AGENT)?;
    let mut token =
        HeaderValue::from_str(session.access_token.expose()).context("the access token is not a valid header value")?;
    token.set_sensitive(true);
    let request = http
        .post(format!("https://{GAMING_HOST}{DISTRIBUTION_PATH}"))
        .header(HeaderName::from_static("x-amz-target"), LIVE_VERSIONS_TARGET)
        .header(HeaderName::from_static("x-amzn-token"), token)
        .header("UserAgent", DISTRIBUTION_USER_AGENT)
        .header("Content-Encoding", "amz-1.0")
        .json(&Request {
            product_ids: &[product_id],
            operation: "GetLiveVersions",
        });
    let answer: Answer = match http::send(request, &LIVE_VERSIONS).await {
        Ok(answer) => answer,
        Err(CallError::Rejected { call, status }) => {
            tracing::info!(
                account_id,
                call,
                status,
                "amazon refused the session for an update check"
            );
            anyhow::bail!("Amazon closed the session: log in to Amazon Games again in Settings");
        }
        Err(err) => {
            let reason = session::reason(err);
            tracing::warn!(account_id, product_id, reason, "amazon did not answer the update check");
            anyhow::bail!("Amazon did not answer the update check: {reason}");
        }
    };
    let live = live_version(account_id, product_id, answer)?;
    let outdated = (live != game.build_id).then_some(live);
    tracing::info!(
        account_id,
        product_id,
        installed = game.build_id,
        outdated = outdated.is_some(),
        "checked the amazon game for an update"
    );
    Ok(outdated)
}

fn live_version(account_id: &str, product_id: &str, answer: Answer) -> anyhow::Result<String> {
    let Some(versions) = answer.versions else {
        tracing::warn!(
            account_id,
            product_id,
            "amazon's live versions answer has no version map"
        );
        anyhow::bail!("Amazon's answer to the update check has no versions");
    };
    let entries = versions.len();
    match versions.get(product_id).map(Value::as_str) {
        Some(Some(version)) if is_id(version) => Ok(version.to_owned()),
        Some(_) => {
            tracing::warn!(
                account_id,
                product_id,
                entries,
                "amazon's live version for the game is not a usable id"
            );
            anyhow::bail!("Amazon's live version for this game is not usable")
        }
        None => {
            tracing::warn!(
                account_id,
                product_id,
                entries,
                "amazon's live versions do not list the game"
            );
            anyhow::bail!("Amazon lists no live version for this game")
        }
    }
}
