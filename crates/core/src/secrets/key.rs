use core::fmt;

use super::{Secret, StoreError};
use crate::catalog::Storefront;

/// A GOG account id written in decimal never needs more digits than this.
const ACCOUNT_ID_MAX_LEN: usize = 20;
const NPID_LEN: core::ops::RangeInclusive<usize> = 3..=16;
/// psport refuses longer shadNet secrets, and so does the launch environment they travel in.
const SHADNET_MAX_LEN: usize = 1024;
const API_KEY_MAX_LEN: usize = 1024;
/// A serialized GOG `Token` is a few hundred bytes; anything near this is not one.
const SESSION_MAX_LEN: usize = 16 << 10;

/// What a secret is, for logs and errors: never its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretKind {
    GogSession,
    ShadnetPassword,
    ShadnetToken,
    SteamGridDbKey,
}

impl fmt::Display for SecretKind {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GogSession => "GOG session",
            Self::ShadnetPassword => "shadNet password",
            Self::ShadnetToken => "shadNet token",
            Self::SteamGridDbKey => "SteamGridDB key",
        })
    }
}

/// A secret of a shadNet account, filed apart from the NPID that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShadnetSecret {
    Password,
    Token,
}

impl ShadnetSecret {
    const fn name(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Token => "token",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Name {
    GogSession(String),
    Shadnet(String, ShadnetSecret),
    SteamGridDb,
}

/// Where one secret is filed.
///
/// Its parts are checked once, here, since they become the name of an entry in the OS keyring;
/// the names are the ones decanter has always used, so nothing stored earlier is lost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretKey(Name);

impl SecretKey {
    pub const STEAMGRIDDB: Self = Self(Name::SteamGridDb);

    /// # Errors
    ///
    /// [`StoreError::InvalidKey`] unless `account_id` is GOG's decimal account id.
    #[inline]
    pub fn gog_session(account_id: &str) -> Result<Self, StoreError> {
        let is_id = (1..=ACCOUNT_ID_MAX_LEN).contains(&account_id.len())
            && account_id.bytes().all(|byte| byte.is_ascii_digit());
        if is_id {
            Ok(Self(Name::GogSession(account_id.to_owned())))
        } else {
            Err(StoreError::InvalidKey {
                kind: SecretKind::GogSession,
            })
        }
    }

    /// # Errors
    ///
    /// [`StoreError::InvalidKey`] unless `npid` is 3 to 16 ASCII letters, digits, `-` or `_`,
    /// the rule psport's launcher and `bbport::online::check_npid` apply.
    #[inline]
    pub fn shadnet(npid: &str, secret: ShadnetSecret) -> Result<Self, StoreError> {
        let is_npid = NPID_LEN.contains(&npid.len())
            && npid
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
        if is_npid {
            Ok(Self(Name::Shadnet(npid.to_owned(), secret)))
        } else {
            Err(StoreError::InvalidKey {
                kind: Self::shadnet_kind(secret),
            })
        }
    }

    const fn shadnet_kind(secret: ShadnetSecret) -> SecretKind {
        match secret {
            ShadnetSecret::Password => SecretKind::ShadnetPassword,
            ShadnetSecret::Token => SecretKind::ShadnetToken,
        }
    }

    #[inline]
    #[must_use]
    pub const fn kind(&self) -> SecretKind {
        match &self.0 {
            Name::GogSession(_) => SecretKind::GogSession,
            Name::Shadnet(_, secret) => Self::shadnet_kind(*secret),
            Name::SteamGridDb => SecretKind::SteamGridDbKey,
        }
    }

    /// The account id or NPID the secret belongs to: identifiers, fit for a log.
    #[inline]
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        match &self.0 {
            Name::GogSession(id) => Some(id),
            Name::Shadnet(npid, _) => Some(npid),
            Name::SteamGridDb => None,
        }
    }

    /// The entry's name under decanter's service in the keyring.
    pub(super) fn entry(&self) -> String {
        match &self.0 {
            Name::GogSession(id) => format!("{id}/{}", Storefront::Gog.as_str_name()),
            Name::Shadnet(npid, secret) => format!("shadnet/{npid}/{}", secret.name()),
            Name::SteamGridDb => "steamgriddb/api_key".to_owned(),
        }
    }

    /// The name this secret was filed under before it had its own: the `SteamGridDB` key went
    /// through the storefront-session API as if it were a session of an unknown storefront.
    pub(super) fn legacy_entry(&self) -> Option<String> {
        match &self.0 {
            Name::SteamGridDb => Some(format!("steamgriddb/{}", Storefront::Unspecified.as_str_name())),
            Name::GogSession(_) | Name::Shadnet(..) => None,
        }
    }

    /// Refuses what the stores would take but nothing that reads it back could use.
    pub(super) fn check(&self, secret: &Secret) -> Result<(), StoreError> {
        let max = match &self.0 {
            Name::GogSession(_) => SESSION_MAX_LEN,
            Name::Shadnet(..) => SHADNET_MAX_LEN,
            Name::SteamGridDb => API_KEY_MAX_LEN,
        };
        let value = secret.expose();
        if value.len() > max {
            return Err(StoreError::TooLarge {
                kind: self.kind(),
                reason: format!("over {max} bytes"),
            });
        }
        if value.contains('\0') {
            return Err(StoreError::Refused {
                kind: self.kind(),
                reason: "holds a NUL character".to_owned(),
            });
        }
        Ok(())
    }
}
