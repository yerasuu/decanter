// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether Epic has published a newer build of an installed game or of one of its installed DLC: the
//! asset list's `buildVersion` for each against the manifest's build version the install recorded
//! (spec, 4.1 and 8.1).
//! That the two name builds the same way is INFER; a build that differs only in how it is spelled
//! reads as an update, whose install then finds nothing to fetch.

use decanter_core::InstalledGame;

use crate::error::CallError;
use crate::http;
use crate::library::assets;
use crate::session::{self, Live};

/// What a check found newer than what is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updates {
    /// Epic's current build of the game, when it differs from the installed one.
    pub game: Option<String>,
    /// The installed DLC whose current build differs from the installed one.
    pub dlcs: Vec<DlcUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlcUpdate {
    pub app_name: String,
    pub title: String,
    pub build_version: String,
}

/// The builds of `game` and of each of its installed DLC that Epic has replaced, from one asset list.
/// A DLC the list does not name, or names without a version, is logged and left out rather than
/// failing the check of the game.
///
/// # Errors
///
/// A closed session, Epic not answering (the asset list is the call seen to stall, spec 12), or an
/// asset list that does not name the game with a version; the reason is fit for the user.
#[inline]
pub async fn check_updates(account_id: &str, game: &InstalledGame) -> anyhow::Result<Updates> {
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
    let current_of = |name: &str| {
        listed
            .list
            .iter()
            .find(|asset| asset.app_name == name)
            .map(|asset| asset.build_version.as_deref())
    };
    let current = current_of(app_name)
        .ok_or_else(|| anyhow::anyhow!("Epic's list of installable games does not name this game"))?
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
    let dlcs = game
        .dlcs
        .iter()
        .filter_map(|(dlc_name, dlc)| {
            let Some(listing) = current_of(dlc_name) else {
                tracing::warn!(
                    account_id,
                    app_name,
                    dlc_name,
                    "the asset list does not name an installed epic dlc"
                );
                return None;
            };
            let Some(dlc_current) = listing else {
                tracing::warn!(
                    account_id,
                    app_name,
                    dlc_name,
                    "the asset list gives an installed epic dlc no version"
                );
                return None;
            };
            let is_dlc_current = dlc_current == dlc.build_id;
            tracing::info!(
                account_id,
                app_name,
                dlc_name,
                installed = dlc.build_id,
                current = dlc_current,
                is_current = is_dlc_current,
                "checked an epic dlc for updates"
            );
            (!is_dlc_current).then(|| DlcUpdate {
                app_name: dlc_name.clone(),
                title: dlc.title.clone(),
                build_version: dlc_current.to_owned(),
            })
        })
        .collect();
    Ok(Updates {
        game: (!is_current).then(|| current.to_owned()),
        dlcs,
    })
}
