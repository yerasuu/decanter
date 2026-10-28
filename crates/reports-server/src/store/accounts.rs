//! The maintainers' accounts (#1153): a third s3db store, `accounts/`, beside the two report partitions and opened
//! by the same passphrase. `users/<id>` holds one account, `idx/name/<name>` makes a name unique (written in the same
//! transaction as the account it names) and `meta/roster.json` counts admins and viewers, so `remove` and `role` can
//! refuse to leave no admin even when two maintainers change accounts at once. Only days are kept, never a time.

use std::sync::Arc;

use anyhow::Result;
use s3db::txn_update::UpdateOp;
use s3db::{Backend, Cipher, Entity, Singleton};
use serde::{Deserialize, Serialize};

use super::{MAX_ATTEMPTS, Store, backoff};
use crate::secrets::{hex, random_hex_256, sha256};

const ROSTER: Singleton = Singleton::new("meta/roster.json");
/// The shape a name must have, for messages.
pub const NAME_RULE: &str = "1 to 32 characters among a-z, 0-9 and -";
const NAME_MAX: usize = 32;
/// The account id in a key: 32 random bits, unique through the create-only write of `users/<id>`.
pub const ID_HEX: usize = 8;
pub const KEY_PREFIX: &str = "drk_";

#[must_use]
pub fn is_name(text: &str) -> bool {
    (1..=NAME_MAX).contains(&text.len()) && text.bytes().all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Lists, rows, attachments, the log viewer, search and diff.
    #[default]
    Viewer,
    /// Also hide, restore, resolve and delete.
    Admin,
}

impl Role {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Admin => "admin",
        }
    }

    pub fn from_name(text: &str) -> Option<Self> {
        [Self::Viewer, Self::Admin].into_iter().find(|role| role.name() == text)
    }
}

/// No `Debug`, so the key's hash cannot reach a log through a stray `{:?}`.
#[derive(Clone, Default, Serialize, Deserialize, Entity)]
#[entity(collection = "users")]
pub struct User {
    #[entity(key)]
    pub id: String,
    pub name: String,
    pub role: Role,
    /// SHA-256 of the whole key, as hex.
    pub key_hash: String,
    /// Raised by every reset, so the server drops the sessions opened with an older key.
    pub generation: u32,
    pub created_day: String,
    pub last_login_day: Option<String>,
    pub disabled: bool,
    /// Set in the transaction that removes the account, because s3db transactions cannot delete: the object and its
    /// name entry go right after, and an account left like this by a crash counts as gone.
    #[serde(default)]
    pub removed: bool,
}

impl User {
    pub const fn is_active(&self) -> bool {
        !self.disabled && !self.removed
    }
}

#[derive(Clone, Default, Serialize, Deserialize, Entity)]
#[entity(collection = "idx/name")]
struct NameEntry {
    #[entity(key)]
    name: String,
    id: String,
}

/// Accounts not removed, by role; kept in the transaction of every add, remove and role change.
#[derive(Clone, Copy, Default, Serialize, Deserialize)]
pub struct Roster {
    pub admins: u32,
    pub viewers: u32,
}

impl Roster {
    const fn slot(&mut self, role: Role) -> &mut u32 {
        match role {
            Role::Admin => &mut self.admins,
            Role::Viewer => &mut self.viewers,
        }
    }

    const fn with(mut self, role: Role) -> Self {
        let slot = self.slot(role);
        *slot = slot.saturating_add(1);
        self
    }

    const fn without(mut self, role: Role) -> Self {
        let slot = self.slot(role);
        *slot = slot.saturating_sub(1);
        self
    }
}

/// What a change to an existing account did.
pub enum Changed {
    Done,
    /// A reset's new key, shown once.
    Key(String),
    NoSuchUser,
    /// The change would leave no admin.
    LastAdmin,
}

/// `drk_<id>_<64 hex>`: the id finds the account without a scan, the 256 random bits are the secret.
fn key_for(id: &str, secret: &str) -> (String, String) {
    let key = format!("{KEY_PREFIX}{id}_{secret}");
    let hash = hex(&sha256(key.as_bytes()));
    (key, hash)
}

fn new_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0_u8; ID_HEX / 2];
    getrandom::fill(&mut bytes)?;
    Ok(hex(&bytes))
}

fn op<T: Serialize>(key: String, value: &T, etag: Option<&str>) -> Result<UpdateOp> {
    match etag {
        Some(etag) => UpdateOp::update(key, value, etag),
        None => UpdateOp::create(key, value),
    }
}

fn user_op(user: &User, etag: Option<&str>) -> Result<UpdateOp> {
    op(User::COLLECTION.key(&user.id)?, user, etag)
}

fn roster_op(roster: Roster, etag: Option<&str>) -> Result<UpdateOp> {
    op(ROSTER.key.to_owned(), &roster, etag)
}

/// A name's entry and, when it still names a live account, that account.
struct Named {
    entry_etag: String,
    user: Option<(User, String)>,
}

#[derive(Clone)]
pub struct AccountStore {
    store: Store,
}

impl AccountStore {
    /// Creates the store's encryption on first use, like a report partition.
    ///
    /// # Errors
    ///
    /// The bucket cannot be reached, or `passphrase` does not open it.
    pub async fn open(backend: Arc<dyn Backend>, passphrase: &str) -> Result<Self> {
        Ok(Self {
            store: Store::open(backend, passphrase).await?,
        })
    }

    /// The roster of an initialised store, read without recovering or creating anything, for `--check-config`.
    ///
    /// # Errors
    ///
    /// The bucket cannot be read, or `passphrase` does not open it.
    pub async fn peek_roster(backend: Arc<dyn Backend>, passphrase: &str) -> Result<Roster> {
        let cipher = s3db::bootstrap::open_cipher(backend.as_ref(), passphrase).await?;
        Self::with_cipher(backend, Arc::new(cipher)).roster().await
    }

    pub const fn with_cipher(backend: Arc<dyn Backend>, cipher: Arc<Cipher>) -> Self {
        Self {
            store: Store::with_cipher(backend, cipher),
        }
    }

    fn backend(&self) -> &dyn Backend {
        self.store.backend()
    }

    /// # Errors
    ///
    /// The bucket fails, or a transaction lock is older than the stale limit and cannot be resolved.
    pub async fn recover(&self) -> Result<usize> {
        self.store.recover().await
    }

    async fn roster_and_etag(&self) -> Result<(Roster, Option<String>)> {
        ROSTER.get(self.backend(), &self.store.cipher).await
    }

    /// # Errors
    ///
    /// The bucket cannot be read.
    pub async fn roster(&self) -> Result<Roster> {
        Ok(self.roster_and_etag().await?.0)
    }

    async fn named(&self, name: &str) -> Result<Option<Named>> {
        let (entry, etag) = NameEntry::get(self.backend(), &self.store.cipher, name).await?;
        let (Some(entry), Some(entry_etag)) = (entry, etag) else {
            return Ok(None);
        };
        let user = self
            .user_and_etag(&entry.id)
            .await?
            .filter(|(user, _)| !user.removed && user.name == name);
        Ok(Some(Named { entry_etag, user }))
    }

    async fn user_and_etag(&self, id: &str) -> Result<Option<(User, String)>> {
        let (user, etag) = User::get(self.backend(), &self.store.cipher, id).await?;
        Ok(user.zip(etag))
    }

    /// One account by id, removed ones excluded: the single-record read of a key the server's map does not know.
    ///
    /// # Errors
    ///
    /// The bucket cannot be read.
    pub async fn user(&self, id: &str) -> Result<Option<User>> {
        Ok(self
            .user_and_etag(id)
            .await?
            .map(|(user, _)| user)
            .filter(|user| !user.removed))
    }

    /// Every account not removed, by name.
    ///
    /// # Errors
    ///
    /// The bucket cannot be listed or an account cannot be read.
    pub async fn users(&self) -> Result<Vec<User>> {
        let ids = User::list_keys(self.backend()).await?;
        let mut users = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(user) = self.user(&id).await? {
                users.push(user);
            }
        }
        users.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(users)
    }

    /// The new account's key, for the caller to show once (it is stored only as its hash); `None` when the name is
    /// taken. Two adds of one name race on the create-only write of its entry, so exactly one of them lands.
    ///
    /// # Errors
    ///
    /// The bucket fails, the random source is unavailable, or the transaction keeps losing races.
    pub async fn add(&self, name: &str, role: Role, today: &str) -> Result<Option<String>> {
        for attempt in 0..MAX_ATTEMPTS {
            let named = self.named(name).await?;
            if named.as_ref().is_some_and(|named| named.user.is_some()) {
                return Ok(None);
            }
            let id = new_id()?;
            let (key, key_hash) = key_for(&id, &random_hex_256()?);
            let user = User {
                id: id.clone(),
                name: name.to_owned(),
                role,
                key_hash,
                created_day: today.to_owned(),
                ..User::default()
            };
            let entry = NameEntry {
                name: name.to_owned(),
                id,
            };
            let (roster, roster_etag) = self.roster_and_etag().await?;
            let ops = vec![
                user_op(&user, None)?,
                op(
                    NameEntry::COLLECTION.key(name)?,
                    &entry,
                    named.as_ref().map(|named| named.entry_etag.as_str()),
                )?,
                roster_op(roster.with(role), roster_etag.as_deref())?,
            ];
            if self.store.commit(ops).await? {
                return Ok(Some(key));
            }
            backoff(attempt).await;
        }
        anyhow::bail!("adding the account kept losing races with other writers")
    }

    /// Applies `edit` to the account named `name` in one transaction with the roster; `edit` answers the new
    /// account and roster, or the outcome to report without writing.
    async fn change(
        &self,
        name: &str,
        mut edit: impl FnMut(&User, Roster) -> Result<(User, Roster, Changed), Changed>,
    ) -> Result<(Changed, Option<User>)> {
        for attempt in 0..MAX_ATTEMPTS {
            let Some((user, etag)) = self.named(name).await?.and_then(|named| named.user) else {
                return Ok((Changed::NoSuchUser, None));
            };
            let (roster, roster_etag) = self.roster_and_etag().await?;
            let (after, new_roster, outcome) = match edit(&user, roster) {
                Ok(planned) => planned,
                Err(outcome) => return Ok((outcome, None)),
            };
            let mut ops = vec![user_op(&after, Some(&etag))?];
            if new_roster.admins != roster.admins || new_roster.viewers != roster.viewers {
                ops.push(roster_op(new_roster, roster_etag.as_deref())?);
            }
            if self.store.commit(ops).await? {
                return Ok((outcome, Some(after)));
            }
            backoff(attempt).await;
        }
        anyhow::bail!("changing the account kept losing races with other writers")
    }

    /// # Errors
    ///
    /// The bucket fails or the transaction keeps losing races.
    pub async fn remove(&self, name: &str) -> Result<Changed> {
        let (outcome, removed) = self
            .change(name, |user, roster| {
                if user.role == Role::Admin && roster.admins <= 1 {
                    return Err(Changed::LastAdmin);
                }
                let after = User {
                    removed: true,
                    ..user.clone()
                };
                Ok((after, roster.without(user.role), Changed::Done))
            })
            .await?;
        if let Some(user) = removed {
            // The removal already landed; a leftover object only keeps the name taken until `add` reclaims it.
            for key in [NameEntry::COLLECTION.key(&user.name)?, User::COLLECTION.key(&user.id)?] {
                if let Err(error) = self.backend().delete(&key).await {
                    tracing::warn!(target: "decanter_reports::accounts", %error, "removed account object not deleted");
                }
            }
        }
        Ok(outcome)
    }

    /// A new key and a new generation; the old key stops working at the server's next refresh.
    ///
    /// # Errors
    ///
    /// The bucket fails, the random source is unavailable, or the transaction keeps losing races.
    pub async fn reset(&self, name: &str) -> Result<Changed> {
        let secret = random_hex_256()?;
        let (outcome, _) = self
            .change(name, |user, roster| {
                let (key, key_hash) = key_for(&user.id, &secret);
                let after = User {
                    key_hash,
                    generation: user.generation.wrapping_add(1),
                    ..user.clone()
                };
                Ok((after, roster, Changed::Key(key)))
            })
            .await?;
        Ok(outcome)
    }

    /// # Errors
    ///
    /// The bucket fails or the transaction keeps losing races.
    pub async fn set_role(&self, name: &str, role: Role) -> Result<Changed> {
        let (outcome, _) = self
            .change(name, |user, roster| {
                if user.role == role {
                    return Err(Changed::Done);
                }
                if user.role == Role::Admin && roster.admins <= 1 {
                    return Err(Changed::LastAdmin);
                }
                let after = User { role, ..user.clone() };
                Ok((after, roster.without(user.role).with(role), Changed::Done))
            })
            .await?;
        Ok(outcome)
    }

    /// Notes the day of a login, once a day per account. Losing a race to a CLI change is fine: the next login of
    /// that day writes it.
    ///
    /// # Errors
    ///
    /// The bucket fails.
    pub async fn record_login(&self, id: &str, today: &str) -> Result<()> {
        let Some((user, etag)) = self.user_and_etag(id).await? else {
            return Ok(());
        };
        if user.removed || user.last_login_day.as_deref() == Some(today) {
            return Ok(());
        }
        let after = User {
            last_login_day: Some(today.to_owned()),
            ..user
        };
        self.store.commit(vec![user_op(&after, Some(&etag))?]).await?;
        Ok(())
    }
}

/// An account store over a fresh local directory with the fixed test key.
#[cfg(test)]
pub fn test_accounts(dir: &std::path::Path) -> AccountStore {
    AccountStore {
        store: super::test_store(dir, s3db::LocalEngine::Redb),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_have_one_shape() {
        for good in ["owner", "a", "ana-2", &"x".repeat(32)] {
            assert!(is_name(good), "{good}");
        }
        for bad in ["", "Owner", "ana_2", "ana 2", "ñ", &"x".repeat(33), "../x"] {
            assert!(!is_name(bad), "{bad}");
        }
    }

    #[test]
    fn keys_carry_the_id_and_256_bits_and_store_only_a_hash() -> Result<(), getrandom::Error> {
        let id = new_id()?;
        assert!(crate::secrets::is_hex_token(&id, ID_HEX));
        let (key, hash) = key_for(&id, &random_hex_256()?);
        let secret = key.strip_prefix(&format!("{KEY_PREFIX}{id}_")).unwrap_or_default();
        assert!(crate::secrets::is_hex_token(secret, 64), "{key}");
        assert!(crate::secrets::matches_hex(&hash, key.as_bytes()));
        assert!(!hash.contains(secret));
        Ok(())
    }
}
