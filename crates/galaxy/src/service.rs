// SPDX-License-Identifier: GPL-3.0-or-later
use core::fmt::Write as _;
use core::future::Future;
use core::net::{Ipv4Addr, SocketAddr};
use core::time::Duration;
use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio::time::sleep;

use crate::codec::{MAX_PAYLOAD_BYTES, read_frame, write_frame};
use crate::error::Error;
use crate::handlers::Connection;
use crate::proto::pb::Header;
use crate::source::{AchievementSource, ClientId};

/// The port every Galaxy SDK build dials; the SDK offers no way to choose another (plan 1.2).
pub const DEFAULT_PORT: u16 = 9977;
/// The plan's cap (2.4): a few games at once, each with a couple of SDK connections.
const MAX_CONNECTIONS: usize = 16;
const BACKLOG: u32 = 16;
/// Pause after a failed accept, so a full descriptor table does not turn the loop into a spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
/// `IDType` user tag in the top byte of an entity id (comet `api.rs:58-89`).
const USER_ID_TAG: u64 = 2 << 56;

/// The signed-in account the SDK is told about.
#[derive(Debug, Clone)]
pub struct Account {
    /// GOG's numeric user id, untagged.
    pub user_id: u64,
    pub user_name: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// [`DEFAULT_PORT`] for games; any other only for a probe.
    pub port: u16,
    /// The one game allowed to sign in, in this stage (plan S1).
    pub client_id: ClientId,
    pub account: Account,
}

pub(crate) struct Shared {
    pub client_id: ClientId,
    pub user_name: String,
    pub account_id: u64,
    /// `account_id` as the SDK sees it, with the user tag.
    pub user_id: u64,
    pub source: Arc<dyn AchievementSource>,
}

pub struct Service {
    listener: TcpListener,
    local_addr: SocketAddr,
    shared: Arc<Shared>,
}

impl Service {
    /// Listens on `127.0.0.1:<port>` only, never on another interface (plan 1.2, requirement 1).
    ///
    /// # Errors
    ///
    /// [`Error::PortInUse`] when another program holds the port, [`Error::Bind`] for any other
    /// socket failure, [`Error::NotLoopback`] if the bound address is somehow not loopback, and
    /// [`Error::UserIdTooLarge`] for an account id the user tag would corrupt.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "listen() registers with tokio's reactor, which exists only inside the runtime: async keeps \
                  every caller there"
    )]
    pub async fn bind(config: Config, source: Arc<dyn AchievementSource>) -> Result<Self, Error> {
        let Config {
            port,
            client_id,
            account,
        } = config;
        if account.user_id >= 1_u64 << 56_u32 {
            return Err(Error::UserIdTooLarge(account.user_id));
        }
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let bind_error = |err: io::Error| {
            if err.kind() == io::ErrorKind::AddrInUse {
                Error::PortInUse(addr)
            } else {
                Error::Bind { addr, source: err }
            }
        };
        let socket = TcpSocket::new_v4().map_err(bind_error)?;
        // SO_REUSEADDR lets a restarted decanter bind past connections still in TIME_WAIT; on
        // Linux it never lets two live listeners share the port. Windows gives it the meaning
        // of SO_REUSEPORT (a second listener could steal the port), so it stays off there, and
        // SO_REUSEPORT is never set anywhere (plan 1.4).
        #[cfg(not(windows))]
        socket.set_reuseaddr(true).map_err(bind_error)?;
        socket.bind(addr).map_err(bind_error)?;
        let listener = socket.listen(BACKLOG).map_err(bind_error)?;
        let local_addr = listener.local_addr().map_err(bind_error)?;
        if !local_addr.ip().is_loopback() {
            return Err(Error::NotLoopback(local_addr));
        }
        tracing::info!(%local_addr, %client_id, "local Galaxy service listening");
        Ok(Self {
            listener,
            local_addr,
            shared: Arc::new(Shared {
                client_id,
                user_name: account.user_name,
                account_id: account.user_id,
                user_id: USER_ID_TAG | account.user_id,
                source,
            }),
        })
    }

    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Serves until `shutdown` completes, then closes the listener and every connection.
    #[expect(
        clippy::integer_division_remainder_used,
        reason = "the `%` is inside tokio::select!'s expansion"
    )]
    pub async fn run<F>(self, shutdown: F)
    where
        F: Future<Output = ()>,
    {
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let mut connections = JoinSet::new();
        let mut refused = 0_u64;
        tokio::pin!(shutdown);
        loop {
            while connections.try_join_next().is_some() {}
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, peer)) => match Arc::clone(&slots).try_acquire_owned() {
                        Ok(permit) => {
                            connections.spawn(serve(stream, peer, Arc::clone(&self.shared), permit));
                        }
                        Err(_no_slot) => {
                            refused = refused.saturating_add(1);
                            tracing::warn!(%peer, refused, cap = MAX_CONNECTIONS, "Galaxy connection refused: too many open");
                        }
                    },
                    Err(err) => {
                        tracing::warn!(%err, "accepting a Galaxy connection failed");
                        sleep(ACCEPT_BACKOFF).await;
                    }
                },
            }
        }
        connections.shutdown().await;
        tracing::info!(local_addr = %self.local_addr, refused, "local Galaxy service stopped");
    }
}

async fn serve(stream: TcpStream, peer: SocketAddr, shared: Arc<Shared>, _slot: OwnedSemaphorePermit) {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut connection = Connection::new(peer, shared);
    let mut ended = "closed by the peer";
    loop {
        let frame = match read_frame::<Header, _>(&mut reader, MAX_PAYLOAD_BYTES).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(err) => {
                tracing::warn!(%peer, %err, "closing a Galaxy connection on a bad frame");
                ended = "bad frame";
                break;
            }
        };
        let (header, payload) = connection.handle(&frame).await;
        if let Err(err) = write_frame(&mut writer, header, &payload).await {
            tracing::warn!(%peer, %err, "writing a Galaxy response failed");
            ended = "write failed";
            break;
        }
    }
    let requests = summary(connection.counts());
    tracing::info!(%peer, ended, %requests, "Galaxy connection ended");
}

/// `sort/type=code xcount` per request kind, the per-connection record plan 2 asks for.
fn summary(counts: &BTreeMap<(u32, u32, i32), u32>) -> String {
    let mut text = String::new();
    for (&(sort, kind, code), count) in counts {
        let separator = if text.is_empty() { "" } else { " " };
        if write!(text, "{separator}{sort}/{kind}={code}x{count}").is_err() {
            break;
        }
    }
    text
}
