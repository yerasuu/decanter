// SPDX-License-Identifier: GPL-3.0-or-later
//! One listener per decanter process, shared by every game it launches, since the port is a
//! host-wide singleton (plan 1.3): the first registration starts it, the last release stops it,
//! and nothing listens while no registered game runs.

use core::sync::atomic::{AtomicUsize, Ordering};
use core::time::Duration;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep};

use crate::error::Error;
use crate::limits::{MAX_CONNECTIONS_PER_GAME, TokenBucket, WRITE_BURST, WRITES_PER_SECOND};
use crate::service::Service;
use crate::source::{ClientId, GameStore};

/// `IDType` user tag in the top byte of an entity id (comet `api.rs:58-89`).
const USER_ID_TAG: u64 = 2 << 56;

/// The signed-in account the SDK is told about.
#[derive(Debug, Clone)]
pub struct Account {
    /// GOG's numeric user id, untagged.
    pub user_id: u64,
    pub user_name: String,
}

/// A game the service lets sign in, for as long as its [`Lease`] lives.
pub(crate) struct Registered {
    pub account_id: u64,
    /// `account_id` as the SDK sees it, with the user tag.
    pub user_id: u64,
    pub user_name: String,
    pub store: Arc<dyn GameStore>,
    /// Signed-in connections, held through [`GameSlot`].
    pub connections: AtomicUsize,
    /// Shared by the game's connections, so that opening more of them buys no more writes. A lock
    /// because the bucket's count and time change together.
    pub writes: Mutex<TokenBucket>,
}

impl Registered {
    /// Waits once for the game's next write token, so that a burst over the budget is slowed
    /// rather than lost; `Err` with the wait left when another connection of the game took it.
    pub(crate) async fn write_budget(&self) -> Result<(), Duration> {
        let Err(wait) = self.take_write() else {
            return Ok(());
        };
        sleep(wait).await;
        self.take_write()
    }

    fn take_write(&self) -> Result<(), Duration> {
        self.writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(Instant::now())
    }
}

/// One of a game's [`MAX_CONNECTIONS_PER_GAME`] signed-in connections, given back on drop.
pub(crate) struct GameSlot {
    game: Arc<Registered>,
}

impl GameSlot {
    pub(crate) fn claim(game: &Arc<Registered>) -> Option<Self> {
        game.connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < MAX_CONNECTIONS_PER_GAME).then(|| held.saturating_add(1))
            })
            .ok()
            .map(|_held| Self { game: Arc::clone(game) })
    }
}

#[expect(
    clippy::missing_trait_methods,
    reason = "pin_drop's default forwards to drop, which is all a slot needs"
)]
impl Drop for GameSlot {
    fn drop(&mut self) {
        let _released = self
            .game
            .connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| held.checked_sub(1));
    }
}

/// Read on every request, written once per launch and exit.
#[derive(Default)]
pub(crate) struct Registry {
    games: RwLock<HashMap<ClientId, Arc<Registered>>>,
}

impl Registry {
    pub(crate) fn get(&self, client_id: &ClientId) -> Option<Arc<Registered>> {
        self.games
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(client_id)
            .map(Arc::clone)
    }

    fn insert(&self, client_id: ClientId, game: Registered) -> Result<(), Error> {
        match self
            .games
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(client_id)
        {
            Entry::Occupied(taken) => Err(Error::AlreadyRegistered(taken.key().clone())),
            Entry::Vacant(free) => {
                free.insert(Arc::new(game));
                Ok(())
            }
        }
    }

    /// How many games are left registered.
    fn remove(&self, client_id: &ClientId) -> usize {
        let mut games = self.games.write().unwrap_or_else(PoisonError::into_inner);
        games.remove(client_id);
        games.len()
    }
}

#[derive(Default)]
struct Listener {
    /// `Some` while serving; taken by the release that empties the registry.
    stop: Option<oneshot::Sender<()>>,
    /// Kept after `stop` is taken, so that the next start waits for the port to be let go.
    task: Option<JoinHandle<()>>,
}

impl Listener {
    fn is_serving(&self) -> bool {
        self.stop.is_some() && self.task.as_ref().is_some_and(|task| !task.is_finished())
    }
}

pub struct Host {
    port: u16,
    registry: Arc<Registry>,
    /// Held across the bind and the wait for a stopping listener, so that two launches at once
    /// never both bind: the second would find its own process holding the port.
    starting: AsyncMutex<()>,
    /// Every registry change and the start or stop it causes happen under this lock, so that the
    /// release of the last game cannot stop a listener a registration has just counted on. A std
    /// lock because [`Lease`] releases from `Drop`, which cannot await.
    listener: Mutex<Listener>,
}

impl Host {
    /// [`crate::DEFAULT_PORT`] for games; any other only for a probe.
    #[must_use]
    #[inline]
    pub fn new(port: u16) -> Arc<Self> {
        Arc::new(Self {
            port,
            registry: Arc::default(),
            starting: AsyncMutex::new(()),
            listener: Mutex::default(),
        })
    }

    /// Lets the game with `client_id` sign in as `account`, served from `store`, starting the
    /// listener if no other game holds it. Must be called inside a tokio runtime, which runs the
    /// listener until the last lease is dropped.
    ///
    /// # Errors
    ///
    /// [`Error::AlreadyRegistered`] when a running game already holds `client_id`, since the
    /// protocol offers no way to tell two of them apart (plan 1.5); [`Error::UserIdTooLarge`];
    /// and the listener's own [`Error::PortInUse`], [`Error::Bind`] and [`Error::NotLoopback`],
    /// after which nothing of this registration is left behind.
    #[inline]
    pub async fn register(
        self: &Arc<Self>,
        client_id: ClientId,
        account: Account,
        store: Arc<dyn GameStore>,
    ) -> Result<Lease, Error> {
        if account.user_id >= 1_u64 << 56_u32 {
            return Err(Error::UserIdTooLarge(account.user_id));
        }
        let _starting = self.starting.lock().await;
        let stopping = {
            let mut listener = self.lock_listener();
            self.registry.insert(
                client_id.clone(),
                Registered {
                    account_id: account.user_id,
                    user_id: USER_ID_TAG | account.user_id,
                    user_name: account.user_name,
                    store,
                    connections: AtomicUsize::new(0),
                    writes: Mutex::new(TokenBucket::new(WRITES_PER_SECOND, WRITE_BURST, Instant::now())),
                },
            )?;
            if listener.is_serving() {
                tracing::info!(%client_id, "game registered with the running local Galaxy service");
                return Ok(self.lease(client_id));
            }
            listener.stop = None;
            listener.task.take()
        };
        // Built before anything can fail or be cancelled, so that its drop undoes the
        // registration on every path that does not hand it out.
        let lease = self.lease(client_id);
        if let Some(task) = stopping
            && let Err(err) = task.await
        {
            tracing::warn!(port = self.port, %err, "the previous local Galaxy listener ended abnormally");
        }
        let service = Service::bind(self.port, Arc::clone(&self.registry)).await?;
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(service.run(async move {
            let _closed = stopped.await;
        }));
        *self.lock_listener() = Listener {
            stop: Some(stop),
            task: Some(task),
        };
        tracing::info!(client_id = %lease.client_id, port = self.port, "game registered, local Galaxy service started");
        Ok(lease)
    }

    fn lease(self: &Arc<Self>, client_id: ClientId) -> Lease {
        Lease {
            host: Arc::clone(self),
            client_id,
        }
    }

    fn release(&self, client_id: &ClientId) {
        let mut listener = self.lock_listener();
        let remaining = self.registry.remove(client_id);
        tracing::info!(%client_id, remaining, "game released from the local Galaxy service");
        if remaining == 0
            && let Some(stop) = listener.stop.take()
        {
            tracing::info!(
                port = self.port,
                "last game released, stopping the local Galaxy service"
            );
            if stop.send(()).is_err() {
                tracing::warn!(port = self.port, "the local Galaxy listener had already ended");
            }
        }
    }

    fn lock_listener(&self) -> MutexGuard<'_, Listener> {
        self.listener.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A game's registration: dropping it, on exit or on any early return of the launch, refuses the
/// game's connections from then on and stops the listener when it was the last.
#[must_use = "dropping the lease unregisters the game at once"]
pub struct Lease {
    host: Arc<Host>,
    client_id: ClientId,
}

#[expect(
    clippy::missing_trait_methods,
    reason = "pin_drop's default forwards to drop, which is all a lease needs"
)]
impl Drop for Lease {
    #[inline]
    fn drop(&mut self) {
        self.host.release(&self.client_id);
    }
}
