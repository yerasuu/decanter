// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether Epic has published a newer build of an installed game: the asset list's `buildVersion`
//! for the game against the manifest's build version the install recorded (spec, 4.1 and 8.1).
//! That the two name builds the same way is INFER; a build that differs only in how it is spelled
//! reads as an update, whose install then finds nothing to fetch.

use decanter_core::InstalledGame;

use crate::error::CallError;
use crate::http;
use crate::library::assets;
use crate::session::{self, Live};

/// `Some(build_version)` when Epic's current build of `game` differs from the one installed, `None`
/// when it is current.
///
/// # Errors
///
/// A closed session, Epic not answering (the asset list is the call seen to stall, spec 12), or an
/// asset list that does not name the game with a version; the reason is fit for the user.
#[inline]
pub async fn check_update(account_id: &str, game: &InstalledGame) -> anyhow::Result<Option<String>> {
    let app_name = game.game_id.as_str();
    let mut session = match session::live(account_id).await? {
        Live::Ready(session) => session,
        Live::Closed => anyhow::bail!("Epic closed the session: log in to Epic Games again in Settings"),
        Live::Unreachable(reason) => anyhow::bail!("Epic could not be asked: {reason}"),
    };
    let client = http::client()?;
    let mut listed = assets::list(&client, session.access_token()).await;
    if matches!(listed, Err(CallError::Revoked { .. })) {
        session = match session::renew_refused(&session).await? {
            Live::Ready(fresh) => fresh,
            Live::Closed => anyhow::bail!("Epic closed the session: log in to Epic Games again in Settings"),
            Live::Unreachable(reason) => anyhow::bail!("Epic could not be asked: {reason}"),
        };
        listed = assets::list(&client, session.access_token()).await;
    }
    let listed = listed.map_err(|err| session::failed("Epic did not list the installable games", err))?;
    let asset = listed
        .list
        .iter()
        .find(|asset| asset.app_name == app_name)
        .ok_or_else(|| anyhow::anyhow!("Epic's list of installable games does not name this game"))?;
    let current = asset
        .build_version
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Epic's list of installable games gives no version for this game"))?;
    let is_current = current == game.build_id;
    tracing::info!(
        account_id,
        app_name,
        installed = game.build_id,
        current,
        is_current,
        "checked an epic game for updates"
    );
    Ok((!is_current).then(|| current.to_owned()))
}
