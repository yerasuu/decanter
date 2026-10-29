// SPDX-License-Identifier: GPL-3.0-or-later
//! The account behind a session and the ids it owns, both on `embed.gog.com`
//! ([gogapidocs: Account](https://gogapidocs.readthedocs.io/en/latest/account.html)).

use serde::Deserialize;

use super::Error;
use super::auth::is_account_id;
use super::http::{Call, Client, Host, SESSION_REFUSED};

/// `userData.json` carries the e-mail address and other personal data, which a parse error must
/// not quote.
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
    /// Decimal digits, checked before it is returned.
    pub id: String,
    pub username: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserData {
    is_logged_in: bool,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Deserialize)]
struct Owned {
    owned: Vec<i64>,
}

impl Client {
    /// The account behind the session, which doubles as the session check.
    ///
    /// # Errors
    ///
    /// [`Error::Rejected`] when GOG refuses the session or says nobody is logged in, even after
    /// one refresh; [`Error::Malformed`] when the account has no numeric id or no username; the
    /// others when GOG cannot be asked.
    #[inline]
    pub async fn user(&self) -> Result<User, Error> {
        self.get(&USER_DATA, Host::Embed, "/userData.json", &[], |data: UserData| {
            // GOG answers a dead session with 200 and `isLoggedIn: false`; as a rejection it also
            // earns the one refresh-and-retry `Client::get` gives a refused call.
            if !data.is_logged_in {
                return Err(Error::Rejected {
                    call: USER_DATA.name,
                    reason: "not logged in".to_owned(),
                });
            }
            match (data.user_id, data.username) {
                (Some(id), Some(username)) if is_account_id(&id) => Ok(User { id, username }),
                _ => Err(Error::Malformed {
                    call: USER_DATA.name,
                    reason: "no numeric user id or no username".to_owned(),
                }),
            }
        })
        .await
    }

    /// The product ids of everything the account owns, in GOG's order, including products the
    /// filtered listing files under no media type.
    ///
    /// # Errors
    ///
    /// As [`Client::user`], without the logged-in check.
    #[inline]
    pub async fn owned_games(&self) -> Result<Vec<i64>, Error> {
        self.get(&OWNED_GAMES, Host::Embed, "/user/data/games", &[], |answer: Owned| {
            Ok(answer.owned)
        })
        .await
    }
}
