//! The maintainers' accounts (#1153): a third s3db store, `accounts/`, beside the two report partitions and opened
//! by the same passphrase. `users/<id>` holds one account, `idx/name/<name>` makes a name unique (written in the same
//! transaction as every change of the account it names) and `meta/roster.json` counts admins and viewers, so
//! `remove` and `role` can refuse to leave no admin even when two maintainers change accounts at once. Accounts keep
//! only days; a removed name's entry keeps the minute of the removal, to hold the name for [`REUSE_HOLD_MINUTES`].

use std::sync::Arc;

use anyhow::Result;
use s3db::txn_update::UpdateOp;
use s3db::{Backend, Cipher, Entity, Singleton};
use serde::{Deserialize, Serialize};

use super::{AuditSummary, MAX_ATTEMPTS, Store, backoff, current_minute};
use crate::secrets::{hex, random_hex_256, sha256};

const ROSTER: Singleton = Singleton::new("meta/roster.json");
/// The shape a name must have, for messages.
pub const NAME_RULE: &str = "1 to 32 characters among a-z, 0-9 and -";
const NAME_MAX: usize = 32;
/// The account id in a key: 32 random bits, unique through the create-only write of `users/<id>`.
pub const ID_HEX: usize = 8;
pub const KEY_PREFIX: &str = "drk_";
/// A removed name is not given to a new account for a day unless the caller insists, because the audit trail keeps
/// naming the old account by it and a new owner of the name would read as the same person.
pub const REUSE_HOLD_MINUTES: u64 = 24 * 60;

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
    /// Set in the transaction that removes the account, because s3db transactions cannot delete: the object goes
    /// right after (its name entry stays as the name's tombstone), and an account left like this by a crash counts
    /// as gone.
    #[serde(default)]
    pub removed: bool,
}

impl User {
    pub const fn is_active(&self) -> bool {
        !self.disabled && !self.removed
    }
}

/// Also mirrors the account's generation and removal, written in the same transaction, so the server can tell
/// whether an account object it cannot read was removed or re-keyed since it last read it.
#[derive(Clone, Default, Serialize, Deserialize, Entity)]
#[entity(collection = "idx/name")]
struct NameEntry {
    #[entity(key)]
    name: String,
    id: String,
    #[serde(default)]
    generation: u32,
    /// The minute (since the epoch) the account was removed; the entry stays as the name's tombstone until an add
    /// reclaims it.
    #[serde(default)]
    removed_minute: Option<u64>,
}

impl NameEntry {
    fn of(user: &User, minute: u64) -> Self {
        Self {
            name: user.name.clone(),
            id: user.id.clone(),
            generation: user.generation,
            removed_minute: user.removed.then_some(minute),
        }
    }
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

/// What an add did.
pub enum Added {
    /// The new account's key, shown once.
    Key(String),
    Taken,
    /// The name was removed less than [`REUSE_HOLD_MINUTES`] ago and the caller did not force its reuse.
    RecentlyRemoved,
}

#[cfg(test)]
impl Added {
    pub fn key(self) -> Option<String> {
        match self {
            Self::Key(key) => Some(key),
            Self::Taken | Self::RecentlyRemoved => None,
        }
    }
}

/// The accounts a listing could read, and the ids of those it could not (a corrupt object, or one the passphrase
/// does not open).
pub struct Listing {
    pub users: Vec<User>,
    pub unreadable: Vec<String>,
}

/// What `--check-config` reads of the accounts store.
pub struct Peeked {
    pub roster: Roster,
    pub audit: AuditSummary,
    /// Account objects listed but not readable.
    pub unreadable: usize,
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

fn entry_op(entry: &NameEntry, etag: Option<&str>) -> Result<UpdateOp> {
    op(NameEntry::COLLECTION.key(&entry.name)?, entry, etag)
}

/// A name's entry and, when it still names a live account, that account.
struct Named {
    entry_etag: String,
    removed_minute: Option<u64>,
    user: Option<(User, String)>,
}

#[derive(Clone)]
pub struct AccountStore {
    pub(super) store: Store,
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

    /// The roster and the audit trail's size of an initialised store, read without recovering or creating
    /// anything, for `--check-config`.
    ///
    /// # Errors
    ///
    /// The bucket cannot be read, or `passphrase` does not open it.
    pub async fn peek(backend: Arc<dyn Backend>, passphrase: &str) -> Result<Peeked> {
        let cipher = s3db::bootstrap::open_cipher(backend.as_ref(), passphrase).await?;
        let store = Self::with_cipher(backend, Arc::new(cipher));
        Ok(Peeked {
            roster: store.roster().await?,
            audit: store.audit_summary().await?,
            unreadable: store.listing().await?.unreadable.len(),
        })
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
        Ok(Some(Named {
            entry_etag,
            removed_minute: entry.removed_minute,
            user,
        }))
    }

    /// Whether `name`'s entry still names the account `id` at `generation`, not removed: what the server trusts
    /// about an account whose own object it cannot read.
    ///
    /// # Errors
    ///
    /// The entry cannot be read.
    pub async fn still_names(&self, name: &str, id: &str, generation: u32) -> Result<bool> {
        let (entry, _) = NameEntry::get(self.backend(), &self.store.cipher, name).await?;
        Ok(entry
            .is_some_and(|entry| entry.id == id && entry.generation == generation && entry.removed_minute.is_none()))
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

    /// Every account not removed, by name, and the ids of the objects that could not be read; one bad object does
    /// not hide the others.
    ///
    /// # Errors
    ///
    /// The bucket cannot be listed.
    pub async fn listing(&self) -> Result<Listing> {
        let ids = User::list_keys(self.backend()).await?;
        let mut users = Vec::with_capacity(ids.len());
        let mut unreadable = Vec::new();
        for id in ids {
            match self.user(&id).await {
                Ok(Some(user)) => users.push(user),
                Ok(None) => {}
                Err(_) => unreadable.push(id),
            }
        }
        users.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Listing { users, unreadable })
    }

    /// Every account not removed, by name.
    ///
    /// # Errors
    ///
    /// The bucket cannot be listed or an account cannot be read.
    pub async fn users(&self) -> Result<Vec<User>> {
        let listing = self.listing().await?;
        if !listing.unreadable.is_empty() {
            anyhow::bail!("{} account object(s) cannot be read", listing.unreadable.len());
        }
        Ok(listing.users)
    }

    /// The new account's key, for the caller to show once (it is stored only as its hash). Two adds of one name race
    /// on the write of its entry, so exactly one of them lands. A name removed less than [`REUSE_HOLD_MINUTES`] ago is
    /// refused unless `force_reuse`.
    ///
    /// # Errors
    ///
    /// The bucket fails, the random source is unavailable, or the transaction keeps losing races.
    pub async fn add(&self, name: &str, role: Role, today: &str, force_reuse: bool) -> Result<Added> {
        self.add_at(name, role, today, current_minute(), force_reuse).await
    }

    async fn add_at(&self, name: &str, role: Role, today: &str, minute: u64, force_reuse: bool) -> Result<Added> {
        for attempt in 0..MAX_ATTEMPTS {
            let named = self.named(name).await?;
            if let Some(named) = &named {
                if named.user.is_some() {
                    return Ok(Added::Taken);
                }
                let held = named
                    .removed_minute
                    .is_some_and(|removed| minute < removed.saturating_add(REUSE_HOLD_MINUTES));
                if held && !force_reuse {
                    return Ok(Added::RecentlyRemoved);
                }
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
            let (roster, roster_etag) = self.roster_and_etag().await?;
            let ops = vec![
                user_op(&user, None)?,
                entry_op(
                    &NameEntry::of(&user, minute),
                    named.as_ref().map(|named| named.entry_etag.as_str()),
                )?,
                roster_op(roster.with(role), roster_etag.as_deref())?,
            ];
            if self.store.commit(ops).await? {
                return Ok(Added::Key(key));
            }
            backoff(attempt).await;
        }
        anyhow::bail!("adding the account kept losing races with other writers")
    }

    /// Applies `edit` to the account named `name` in one transaction with its name entry and the roster; `edit`
    /// answers the new account and roster, or the outcome to report without writing.
    async fn change(
        &self,
        name: &str,
        minute: u64,
        mut edit: impl FnMut(&User, Roster) -> Result<(User, Roster, Changed), Changed>,
    ) -> Result<(Changed, Option<User>)> {
        for attempt in 0..MAX_ATTEMPTS {
            let Some((entry_etag, (user, etag))) = self
                .named(name)
                .await?
                .and_then(|named| named.user.map(|user| (named.entry_etag, user)))
            else {
                return Ok((Changed::NoSuchUser, None));
            };
            let (roster, roster_etag) = self.roster_and_etag().await?;
            let (after, new_roster, outcome) = match edit(&user, roster) {
                Ok(planned) => planned,
                Err(outcome) => return Ok((outcome, None)),
            };
            let mut ops = vec![
                user_op(&after, Some(&etag))?,
                entry_op(&NameEntry::of(&after, minute), Some(&entry_etag))?,
            ];
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
        let (outcome, removed) = self.mark_removed(name, current_minute()).await?;
        if let Some(user) = removed {
            self.forget(&user.id).await;
        }
        Ok(outcome)
    }

    /// The transaction of a removal: the account marked removed, its name entry turned into the name's tombstone
    /// and the roster, together.
    async fn mark_removed(&self, name: &str, minute: u64) -> Result<(Changed, Option<User>)> {
        self.change(name, minute, |user, roster| {
            if user.role == Role::Admin && roster.admins <= 1 {
                return Err(Changed::LastAdmin);
            }
            let after = User {
                removed: true,
                ..user.clone()
            };
            Ok((after, roster.without(user.role), Changed::Done))
        })
        .await
    }

    /// Deletes a removed account's object once its removal has landed. The name entry is left alone: by now it may
    /// already name a newer account, and otherwise it is the tombstone that holds the name. An object that is not
    /// marked removed is never deleted. A failure only leaves an object that already counts as gone.
    async fn forget(&self, id: &str) {
        let removed = match self.user_and_etag(id).await {
            Ok(found) => found.is_some_and(|(user, _)| user.id == id && user.removed),
            Err(error) => {
                tracing::warn!(target: "decanter_reports::accounts", %error, "removed account object not read back");
                false
            }
        };
        if !removed {
            return;
        }
        let deleted = match User::COLLECTION.key(id) {
            Ok(key) => self.backend().delete(&key).await.map(|_| ()),
            Err(error) => Err(error),
        };
        if let Err(error) = deleted {
            tracing::warn!(target: "decanter_reports::accounts", %error, "removed account object not deleted");
        }
    }

    /// A new key and a new generation; the old key stops working at the server's next refresh.
    ///
    /// # Errors
    ///
    /// The bucket fails, the random source is unavailable, or the transaction keeps losing races.
    pub async fn reset(&self, name: &str) -> Result<Changed> {
        let secret = random_hex_256()?;
        let (outcome, _) = self
            .change(name, current_minute(), |user, roster| {
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
            .change(name, current_minute(), |user, roster| {
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

#[cfg(test)]
impl AccountStore {
    /// Replaces an account's object with bytes that do not decrypt, as a corrupt object would read.
    pub async fn overwrite_raw(&self, id: &str, bytes: &[u8]) -> Result<()> {
        let key = User::COLLECTION.key(id)?;
        let (_, etag) = self.backend().get(&key).await?;
        self.backend().put_cas(&key, bytes.to_vec(), etag.as_deref()).await?;
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

    type Test = Result<(), Box<dyn std::error::Error>>;
    const DAY: &str = "2026-10-29";
    const MINUTE: u64 = 29_000_000;

    fn fresh() -> Result<(AccountStore, std::path::PathBuf), getrandom::Error> {
        let dir = std::env::temp_dir().join(format!("decanter-reports-accounts-{}", crate::secrets::random_hex()?));
        Ok((test_accounts(&dir), dir))
    }

    #[tokio::test]
    async fn a_removal_cleanup_that_runs_after_the_name_is_reused_leaves_the_new_account_whole() -> Test {
        let (store, dir) = fresh()?;
        store
            .add_at("owner", Role::Admin, DAY, MINUTE, false)
            .await?
            .key()
            .ok_or("owner")?;
        store
            .add_at("ana", Role::Viewer, DAY, MINUTE, false)
            .await?
            .key()
            .ok_or("ana")?;
        let (_, removed) = store.mark_removed("ana", MINUTE).await?;
        let old = removed.ok_or("ana was removed")?;
        let new_key = store
            .add_at("ana", Role::Viewer, DAY, MINUTE, true)
            .await?
            .key()
            .ok_or("new ana")?;
        let new_id = new_key.get(4..12).unwrap_or_default().to_owned();
        store.forget(&old.id).await;

        let named = store
            .named("ana")
            .await?
            .ok_or("ana's entry survives the old cleanup")?;
        assert_eq!(named.user.map(|(user, _)| user.id), Some(new_id.clone()));
        assert!(store.user(&new_id).await?.is_some());
        assert!(
            store.user_and_etag(&old.id).await?.is_none(),
            "the removed object is gone"
        );
        assert!(store.still_names("ana", &new_id, 0).await? && !store.still_names("ana", &old.id, 0).await?);
        store.forget(&new_id).await;
        assert!(
            store.user(&new_id).await?.is_some(),
            "a live account is never forgotten"
        );
        let roster = store.roster().await?;
        assert_eq!((roster.admins, roster.viewers), (1, 1));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn a_removed_name_is_held_for_a_day_unless_forced() -> Test {
        let (store, dir) = fresh()?;
        store
            .add_at("owner", Role::Admin, DAY, MINUTE, false)
            .await?
            .key()
            .ok_or("owner")?;
        store
            .add_at("bo", Role::Viewer, DAY, MINUTE, false)
            .await?
            .key()
            .ok_or("bo")?;
        store.mark_removed("bo", MINUTE).await?.1.ok_or("bo was removed")?;
        let held = store
            .add_at("bo", Role::Viewer, DAY, MINUTE + REUSE_HOLD_MINUTES - 1, false)
            .await?;
        assert!(matches!(held, Added::RecentlyRemoved));
        let later = store
            .add_at("bo", Role::Viewer, DAY, MINUTE + REUSE_HOLD_MINUTES, false)
            .await?;
        assert!(matches!(later, Added::Key(_)));
        assert!(matches!(
            store.add_at("bo", Role::Viewer, DAY, MINUTE, true).await?,
            Added::Taken
        ));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn an_unreadable_account_object_is_listed_apart_and_the_rest_still_read() -> Test {
        let (store, dir) = fresh()?;
        store
            .add_at("owner", Role::Admin, DAY, MINUTE, false)
            .await?
            .key()
            .ok_or("owner")?;
        let ana = store
            .add_at("ana", Role::Viewer, DAY, MINUTE, false)
            .await?
            .key()
            .ok_or("ana")?;
        let ana_id = ana.get(4..12).unwrap_or_default();
        store.overwrite_raw(ana_id, b"not a sealed object").await?;
        let listing = store.listing().await?;
        assert_eq!(listing.unreadable, vec![ana_id.to_owned()]);
        assert_eq!(
            listing.users.iter().map(|user| user.name.as_str()).collect::<Vec<_>>(),
            ["owner"]
        );
        assert!(store.users().await.is_err(), "the CLI's list still refuses to hide one");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
