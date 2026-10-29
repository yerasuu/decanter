//! The server's view of the maintainers' accounts: a map by id, read whole from the `accounts/` store at start and
//! every [`REFRESH_EVERY`], so `user add`, `remove`, `reset` and `role` take effect without a restart. A refresh also
//! ends the sessions of accounts removed, disabled or re-keyed since they logged in.
//!
//! A key is `drk_<id>_<64 hex>`: its id finds the account and the whole key is compared, in constant time, against
//! the stored SHA-256. An unknown id is compared against a dummy hash and answers the same 404. It is not free: the
//! first time an id is unknown the server reads the bucket for it (so an account added since the last refresh works
//! at once), and that read takes longer than a known id's lookup. A miss is remembered for [`MISS_TTL`], so repeating
//! an unknown id costs no further read and, for that time, the in-memory work of a wrong secret.

pub mod cli;

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use crate::app::AppState;
use crate::secrets::{is_hex_token, matches_hex};
use crate::store::{AccountStore, ID_HEX, KEY_PREFIX, Role, User};

pub const REFRESH_EVERY: Duration = Duration::from_secs(15);
const SECRET_HEX: usize = 64;
/// Compared when no account answers, so that path does the same hashing as a wrong secret.
const DUMMY_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MISS_TTL: Duration = Duration::from_secs(30);
/// Bounds the misses an attacker rotating ids can make the server remember.
const MISS_CAP: usize = 1024;

/// Who a key or session belongs to, as the gate passes it on.
#[derive(Clone)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub generation: u32,
}

#[derive(Clone)]
struct Known {
    identity: Identity,
    key_hash: String,
    active: bool,
}

impl Known {
    fn of(user: User) -> Self {
        let active = user.is_active();
        Self {
            identity: Identity {
                id: user.id,
                name: user.name,
                role: user.role,
                generation: user.generation,
            },
            key_hash: user.key_hash,
            active,
        }
    }
}

/// The account id of a key of the right shape; `None` for anything else.
fn key_id(key: &str) -> Option<&str> {
    let (id, secret) = key.strip_prefix(KEY_PREFIX)?.split_once('_')?;
    (is_hex_token(id, ID_HEX) && is_hex_token(secret, SECRET_HEX)).then_some(id)
}

/// Ids the bucket was asked for and did not hold, so repeating one costs no read.
#[derive(Default)]
struct Misses(Mutex<HashMap<String, Instant>>);

impl Misses {
    fn contains(&self, id: &str, now: Instant) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .is_some_and(|at| now.saturating_duration_since(*at) < MISS_TTL)
    }

    fn insert(&self, id: &str, now: Instant) {
        let mut misses = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if misses.len() >= MISS_CAP {
            misses.retain(|_, at| now.saturating_duration_since(*at) < MISS_TTL);
        }
        if misses.len() >= MISS_CAP
            && let Some(oldest) = misses.iter().min_by_key(|(_, at)| **at).map(|(id, _)| id.clone())
        {
            misses.remove(&oldest);
        }
        misses.insert(id.to_owned(), now);
        drop(misses);
    }

    fn clear(&self) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }
}

pub struct Accounts {
    store: AccountStore,
    by_id: RwLock<HashMap<String, Known>>,
    misses: Misses,
}

impl Accounts {
    #[must_use]
    pub fn new(store: AccountStore) -> Self {
        Self {
            store,
            by_id: RwLock::new(HashMap::new()),
            misses: Misses::default(),
        }
    }

    pub const fn store(&self) -> &AccountStore {
        &self.store
    }

    /// Replaces the map with what the store holds now. An account object that cannot be read keeps its previous
    /// entry only while its name entry, written in the same transaction as every change, still names it with the
    /// generation the server knew and not removed; otherwise it is dropped, so a removal or a reset is never held
    /// back by a corrupt object. Each such object is one warning per refresh.
    ///
    /// # Errors
    ///
    /// The store cannot be listed; the map is left as it was.
    pub async fn refresh(&self) -> anyhow::Result<()> {
        let listing = self.store.listing().await?;
        let mut map: HashMap<String, Known> = listing
            .users
            .into_iter()
            .map(|user| (user.id.clone(), Known::of(user)))
            .collect();
        let mut previous: HashMap<String, Known> = {
            let by_id = self.by_id.read().unwrap_or_else(PoisonError::into_inner);
            listing
                .unreadable
                .iter()
                .filter_map(|id| by_id.get(id).map(|known| (id.clone(), known.clone())))
                .collect()
        };
        for id in &listing.unreadable {
            let kept = match previous.remove(id) {
                Some(known) => {
                    let vouched = self
                        .store
                        .still_names(&known.identity.name, id, known.identity.generation)
                        .await;
                    let kept = matches!(vouched, Ok(true));
                    if kept {
                        map.insert(id.clone(), known);
                    }
                    kept
                }
                None => false,
            };
            tracing::warn!(
                target: "decanter_reports::accounts",
                account = %id,
                kept,
                "account object unreadable: kept only while its name entry still names it unchanged"
            );
        }
        *self.by_id.write().unwrap_or_else(PoisonError::into_inner) = map;
        self.misses.clear();
        Ok(())
    }

    /// Role and generation of an active account, for the sessions' check after a refresh.
    pub fn current(&self, id: &str) -> Option<(Role, u32)> {
        self.by_id
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .filter(|known| known.active)
            .map(|known| (known.identity.role, known.identity.generation))
    }

    pub fn has_admin(&self) -> bool {
        self.by_id
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .any(|known| known.active && known.identity.role == Role::Admin)
    }

    fn lookup(&self, id: &str) -> Option<(String, Identity, bool)> {
        self.by_id
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .map(|known| (known.key_hash.clone(), known.identity.clone(), known.active))
    }

    /// The one read a key with an id the map does not know may cost, so an account added since the last refresh
    /// works at once; an id the bucket does not hold is remembered as a miss. A failed read is a miss too, not
    /// remembered: the request still answers the uniform 404.
    async fn read_one(&self, id: &str) -> Option<(String, Identity, bool)> {
        match self.store.user(id).await {
            Ok(Some(user)) => {
                let known = Known::of(user);
                let found = (known.key_hash.clone(), known.identity.clone(), known.active);
                self.by_id
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(id.to_owned(), known);
                Some(found)
            }
            Ok(None) => {
                self.misses.insert(id, Instant::now());
                None
            }
            Err(error) => {
                tracing::warn!(target: "decanter_reports::accounts", %error, "account read failed");
                None
            }
        }
    }

    /// The account this key opens. An unknown id, a wrong secret, a disabled or removed account and a key of the
    /// wrong shape all answer `None` after the same comparison.
    pub async fn verify(&self, key: &str) -> Option<Identity> {
        let found = match key_id(key) {
            Some(id) => match self.lookup(id) {
                Some(found) => Some(found),
                None if self.misses.contains(id, Instant::now()) => None,
                None => self.read_one(id).await,
            },
            None => None,
        };
        let hash = found.as_ref().map_or(DUMMY_HASH, |(hash, ..)| hash.as_str());
        let matched = matches_hex(hash, key.as_bytes());
        found
            .filter(|(_, _, active)| matched && *active)
            .map(|(_, identity, _)| identity)
    }

    /// Notes the login's day on the account; a failure is logged and the login goes on.
    pub async fn record_login(&self, id: &str) {
        if let Err(error) = self.store.record_login(id, &crate::app::today()).await {
            tracing::warn!(target: "decanter_reports::accounts", %error, "last login day not recorded");
        }
    }
}

/// Re-reads the accounts and ends the sessions they no longer back.
///
/// # Errors
///
/// The store cannot be read; the map and the sessions are left as they were.
pub async fn refresh_now(state: &AppState) -> anyhow::Result<()> {
    state.accounts.refresh().await?;
    state.sessions.revalidate(|id| state.accounts.current(id));
    Ok(())
}

/// [`refresh_now`] every [`REFRESH_EVERY`]. A failed listing keeps the previous map, so a bucket outage neither logs
/// everyone out nor lets a removed account back in.
pub async fn refresh_loop(state: AppState) {
    loop {
        tokio::time::sleep(REFRESH_EVERY).await;
        if let Err(error) = refresh_now(&state).await {
            tracing::warn!(target: "decanter_reports::accounts", %error, "account refresh failed");
        }
    }
}

#[cfg(test)]
pub const TEST_ADMIN_KEY: &str = "drk_00000a01_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
#[cfg(test)]
pub const TEST_VIEWER_KEY: &str = "drk_00000b01_fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

#[cfg(test)]
impl Accounts {
    /// Puts an account straight into the map, not the store, so a synchronous test harness has fixed keys; a
    /// refresh forgets it.
    pub fn seed(&self, key: &str, name: &str, role: Role) {
        let id = key_id(key).unwrap_or_default().to_owned();
        let known = Known {
            identity: Identity {
                id: id.clone(),
                name: name.to_owned(),
                role,
                generation: 0,
            },
            key_hash: crate::secrets::hex(&crate::secrets::sha256(key.as_bytes())),
            active: true,
        };
        self.by_id
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, known);
    }
}

/// Accounts over `<root>/accounts` holding `owner` (admin, [`TEST_ADMIN_KEY`]) and `ana` (viewer,
/// [`TEST_VIEWER_KEY`]) in the map only.
#[cfg(test)]
pub fn test_accounts(root: &std::path::Path) -> Accounts {
    let accounts = Accounts::new(crate::store::test_accounts(&root.join("accounts")));
    accounts.seed(TEST_ADMIN_KEY, "owner", Role::Admin);
    accounts.seed(TEST_VIEWER_KEY, "ana", Role::Viewer);
    accounts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_well_formed_key_names_an_id() {
        let secret = "a".repeat(64);
        assert_eq!(key_id(&format!("drk_0123abcd_{secret}")), Some("0123abcd"));
        for bad in [
            format!("drk_0123abc_{secret}"),
            format!("drk_0123ABCD_{secret}"),
            format!("drk_0123abcd_{}", "a".repeat(63)),
            format!("drk_0123abcd{secret}"),
            format!("drx_0123abcd_{secret}"),
            String::new(),
        ] {
            assert_eq!(key_id(&bad), None, "{bad}");
        }
    }

    #[test]
    fn misses_expire_and_stay_bounded() {
        let misses = Misses::default();
        let t0 = Instant::now();
        misses.insert("0000ffff", t0);
        assert!(misses.contains("0000ffff", t0 + Duration::from_secs(29)));
        assert!(!misses.contains("0000ffff", t0 + MISS_TTL));
        for n in 0..MISS_CAP + 10 {
            misses.insert(&format!("{n:08x}"), t0 + Duration::from_millis(n as u64));
        }
        assert_eq!(misses.0.lock().map(|map| map.len()).unwrap_or_default(), MISS_CAP);
        assert!(!misses.contains("00000000", t0), "the oldest went first");
        assert!(misses.contains(&format!("{:08x}", MISS_CAP + 9), t0));
    }

    type Test = Result<(), Box<dyn std::error::Error>>;

    fn fresh() -> Result<(Accounts, std::path::PathBuf), getrandom::Error> {
        let dir = std::env::temp_dir().join(format!("decanter-reports-refresh-{}", crate::secrets::random_hex()?));
        Ok((Accounts::new(crate::store::test_accounts(&dir)), dir))
    }

    async fn add(accounts: &Accounts, name: &str, role: Role) -> Result<String, Box<dyn std::error::Error>> {
        Ok(accounts
            .store()
            .add(name, role, "2026-10-29", false)
            .await?
            .key()
            .ok_or_else(|| name.to_owned())?)
    }

    fn id_of(key: &str) -> &str {
        key_id(key).unwrap_or_default()
    }

    #[tokio::test]
    async fn an_unreadable_account_never_outlives_its_removal_or_reset() -> Test {
        let (accounts, dir) = fresh()?;
        let owner = add(&accounts, "owner", Role::Admin).await?;
        let ana = add(&accounts, "ana", Role::Viewer).await?;
        let bo = add(&accounts, "bo", Role::Viewer).await?;
        let cy = add(&accounts, "cy", Role::Viewer).await?;
        accounts.refresh().await?;
        let store = accounts.store();
        store.reset("ana").await?;
        store.remove("cy").await?;
        for key in [&ana, &bo, &cy] {
            store.overwrite_raw(id_of(key), b"not a sealed object").await?;
        }
        accounts.refresh().await?;
        assert!(accounts.verify(&owner).await.is_some());
        assert!(
            accounts.verify(&bo).await.is_some(),
            "an unchanged account keeps its entry while its object is unreadable"
        );
        assert!(accounts.verify(&ana).await.is_none(), "the reset is not held back");
        assert!(accounts.verify(&cy).await.is_none(), "the removal is not held back");
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[tokio::test]
    async fn an_unknown_id_is_read_once_and_then_remembered() -> Test {
        let (accounts, dir) = fresh()?;
        let unknown = format!("drk_0000ffff_{}", "a".repeat(64));
        assert!(accounts.verify(&unknown).await.is_none());
        assert!(accounts.misses.contains("0000ffff", Instant::now()));
        let late = add(&accounts, "late", Role::Viewer).await?;
        assert!(
            accounts.verify(&late).await.is_some(),
            "a miss of another id does not hide a new account"
        );
        accounts.refresh().await?;
        assert!(
            !accounts.misses.contains("0000ffff", Instant::now()),
            "a refresh forgets the misses"
        );
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
