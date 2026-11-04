//! The local Galaxy service around one launch: decanter's process-wide listener on
//! `127.0.0.1:9977`, the game registered by its Galaxy client id before it starts and released
//! when the caller drops the lease (docs/galaxy-local-service-plan.md, sections 1.3, 1.4 and 3.1).

use core::time::Duration;
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::sync::{Arc, LazyLock};

use decanter_core::{Db, Storefront};
use decanter_galaxy::{Account, DEFAULT_PORT, Error, Host, Lease, log_id};
use tokio::time::timeout;

use crate::GalaxyStore;
use crate::install::galaxy_client_id;

/// One per process, because the port is one per machine (plan 1.3).
static HOST: LazyLock<Arc<Host>> = LazyLock::new(|| Host::new(DEFAULT_PORT));

/// The longest a launch waits on GOG for the achievements and stats it seeds; past it the game
/// starts with what the tables hold (plan 3.1).
const SEED_TIMEOUT: Duration = Duration::from_secs(10);

/// Registers a GOG game with the local Galaxy service, then seeds it from GOG, bounded.
///
/// `product_id` of `account_id` is installed at `install_root`; its achievements and stats are
/// refreshed for at most [`SEED_TIMEOUT`]. The game must be started only after this returns, and
/// the lease held until it exits.
///
/// `None`, with the reason logged, whenever the game goes without the service and still starts:
/// the service turned off in settings, a build without Galaxy, an account id GOG did not issue,
/// the port held by another program (a real Galaxy client, another decanter), or the same game
/// already running under another profile. Seeding is skipped when the service is not started.
#[inline]
pub async fn serve_galaxy(db_path: &Path, account_id: &str, product_id: &str, install_root: &Path) -> Option<Lease> {
    let mut db = match Db::open(db_path) {
        Ok(db) => db,
        Err(err) => {
            tracing::warn!(product_id = %log_id(product_id), %err, "could not open the database, the game starts without the local Galaxy service");
            return None;
        }
    };
    match db.galaxy_service_enabled() {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(product_id = %log_id(product_id), "local Galaxy service turned off in settings, not started");
            return None;
        }
        Err(err) => {
            tracing::warn!(product_id = %log_id(product_id), %err, "could not read the local Galaxy service setting, not started");
            return None;
        }
    }
    let Some(client_id) = galaxy_client_id(install_root, product_id) else {
        tracing::debug!(
            product_id = %log_id(product_id),
            "no Galaxy client id in the install, local Galaxy service not applicable"
        );
        return None;
    };
    let account = account(&db, account_id, product_id)?;
    let store = GalaxyStore::new(
        db_path.to_owned(),
        account_id.to_owned(),
        client_id.clone(),
        product_id.to_owned(),
    );
    let lease = match HOST.register(client_id.clone(), account, Arc::new(store)).await {
        Ok(lease) => lease,
        Err(Error::PortInUse(addr)) => {
            tracing::warn!(
                product_id = %log_id(product_id),
                %client_id,
                %addr,
                "another program (a Galaxy client, another decanter) holds the Galaxy port: local Galaxy service \
                 not started, achievements and stats of this run are not tracked by decanter"
            );
            return None;
        }
        Err(err) => {
            tracing::warn!(product_id = %log_id(product_id), %client_id, %err, "local Galaxy service not started for this game");
            return None;
        }
    };
    seed(&mut db, account_id, product_id).await;
    Some(lease)
}

/// What the local Galaxy service does for a game, read without starting or changing anything.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GalaxyStatus {
    /// The service is, or at the next launch will be, up for the game.
    Active,
    /// Something else, not this process, holds the Galaxy port, so a launch goes untracked.
    PortHeld,
    /// The install has no Galaxy client id: its build does not use the SDK.
    NotAvailable,
    TurnedOff,
}

/// The status a launch of `product_id`, installed at `install_root`, would find.
///
/// The port is probed with a bind that is dropped at once, the one thing that tells a free port
/// from a held one before any game runs; a launch racing the probe can in theory see the port
/// busy for that instant, and then goes untracked as it would for any other holder.
#[must_use]
#[inline]
pub fn galaxy_status(db: &Db, install_root: &Path, product_id: &str) -> GalaxyStatus {
    if galaxy_client_id(install_root, product_id).is_none() {
        return GalaxyStatus::NotAvailable;
    }
    match db.galaxy_service_enabled() {
        Ok(true) => {}
        Ok(false) => return GalaxyStatus::TurnedOff,
        Err(err) => {
            tracing::warn!(product_id = %log_id(product_id), %err, "could not read the local Galaxy service setting");
            return GalaxyStatus::TurnedOff;
        }
    }
    if HOST.is_serving() {
        return GalaxyStatus::Active;
    }
    match TcpListener::bind((Ipv4Addr::LOCALHOST, DEFAULT_PORT)) {
        Ok(_free) => GalaxyStatus::Active,
        Err(err) => {
            tracing::debug!(product_id = %log_id(product_id), %err, "the Galaxy port cannot be bound");
            GalaxyStatus::PortHeld
        }
    }
}

/// What the SDK is told about the signed-in user; `None` for an account id GOG did not issue.
fn account(db: &Db, account_id: &str, product_id: &str) -> Option<Account> {
    let Ok(user_id) = account_id.parse::<u64>() else {
        tracing::warn!(
            account_id = %log_id(account_id),
            product_id = %log_id(product_id),
            "account id is not a GOG user id, local Galaxy service not started"
        );
        return None;
    };
    let user_name = match db.account_name(account_id, Storefront::Gog) {
        Ok(Some(name)) => name,
        Ok(None) => {
            tracing::warn!(
                account_id = %log_id(account_id),
                product_id = %log_id(product_id),
                "no stored name for the account, the SDK is told an empty one"
            );
            String::new()
        }
        Err(err) => {
            tracing::warn!(account_id = %log_id(account_id), product_id = %log_id(product_id), %err, "could not read the account name, the SDK is told an empty one");
            String::new()
        }
    };
    Some(Account { user_id, user_name })
}

/// Best effort and bounded, so that a slow or unreachable GOG never holds the launch: the service
/// answers from whatever the tables hold, and neither refresh lowers local progress (plan 3.2).
async fn seed(db: &mut Db, account_id: &str, product_id: &str) {
    let seeding = async {
        if let Err(err) = crate::poll_achievements(db, account_id, product_id).await {
            tracing::warn!(
                account_id = %log_id(account_id),
                product_id = %log_id(product_id),
                err = format!("{err:#}"),
                "achievements not refreshed before launch"
            );
        }
        if let Err(err) = crate::seed_user_stats(db, account_id, product_id).await {
            tracing::warn!(
                account_id = %log_id(account_id),
                product_id = %log_id(product_id),
                err = format!("{err:#}"),
                "user stats not refreshed before launch"
            );
        }
    };
    if timeout(SEED_TIMEOUT, seeding).await.is_err() {
        tracing::warn!(
            account_id = %log_id(account_id),
            product_id = %log_id(product_id),
            timeout_secs = SEED_TIMEOUT.as_secs(),
            "seeding from GOG took too long, the game starts with what is stored"
        );
    }
}
