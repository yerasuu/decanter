// SPDX-License-Identifier: GPL-3.0-only
// Based on gog 0.5.0 (gogapi-rs) by nicohman: the `embed.gog.com` user endpoints come from its
// `Gog::get_user_data` and `Gog::get_games`; what counts as logged out from decanter-gog's
// `session.rs`.
//! Who the session belongs to and what it owns.

use serde::Deserialize;

use super::http::{Call, Client, Host, SESSION_REFUSED};
use super::{auth, Error};

/// The address is personal data, so a parse error must not quote the body.
const USER_DATA: Call = Call {
    name: "user data",
    refused: SESSION_REFUSED,
    max_body: 2 << 20,
    has_secrets: true,
};

const OWNED_GAMES: Call = Call {
    name: "owned games",
    refused: SESSION_REFUSED,
    max_body: 2 << 20,
    has_secrets: false,
};

/// The account a session belongs to.
#[non_exhaustive]
pub struct User {
    /// The GOG account id, decimal digits.
    pub id: String,
    pub username: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserData {
    is_logged_in: bool,
    user_id: Option<String>,
    username: Option<String>,
}

#[derive(Deserialize)]
struct OwnedGames {
    owned: Vec<i64>,
}

impl Client {
    /// The account behind this session, which also tells whether GOG still accepts it.
    ///
    /// # Errors
    ///
    /// [`Error::Rejected`] when GOG no longer accepts the session, refresh included; the others
    /// when GOG cannot be asked or answers something unreadable.
    #[inline]
    pub async fn user(&self) -> Result<User, Error> {
        self.get(
            &USER_DATA,
            Host::Embed,
            "/userData.json",
            &[],
            |data: UserData| {
                if !data.is_logged_in {
                    return Err(Error::Rejected {
                        call: USER_DATA.name,
                        reason: "not logged in".to_owned(),
                    });
                }
                match (data.user_id, data.username) {
                    (Some(id), Some(username)) if auth::is_account_id(&id) => {
                        Ok(User { id, username })
                    }
                    _ => Err(Error::Malformed {
                        call: USER_DATA.name,
                        reason: "no numeric user id or no username".to_owned(),
                    }),
                }
            },
        )
        .await
    }

    /// The product ids of everything the account owns, games or not.
    ///
    /// # Errors
    ///
    /// As [`Client::user`].
    #[inline]
    pub async fn owned_games(&self) -> Result<Vec<i64>, Error> {
        self.get(
            &OWNED_GAMES,
            Host::Embed,
            "/user/data/games",
            &[],
            |games: OwnedGames| Ok(games.owned),
        )
        .await
    }
}
