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
use tokio::task::{JoinSet, spawn_blocking};
use tokio::time::{Instant, sleep, timeout_at};

use crate::codec::{MAX_PAYLOAD_BYTES, read_frame, write_frame};
use crate::error::Error;
use crate::handlers::{Connection, OTHER_KINDS};
use crate::host::Registry;
use crate::limits::{
    MAX_CONNECTIONS, MAX_UNAUTHENTICATED, REQUEST_BURST, REQUESTS_PER_SECOND, Refusal, Refusals, SIGN_IN_DEADLINE,
    TokenBucket,
};
use crate::peer::{self, Verdict};
use crate::proto::pb::Header;

/// The port every Galaxy SDK build dials; the SDK offers no way to choose another (plan 1.2).
pub const DEFAULT_PORT: u16 = 9977;
const BACKLOG: u32 = 16;
/// Pause after a failed accept, so a full descriptor table does not turn the loop into a spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

pub(crate) struct Service {
    listener: TcpListener,
    local_addr: SocketAddr,
    registry: Arc<Registry>,
}

impl Service {
    /// Listens on `127.0.0.1:<port>` only, never on another interface (plan 1.2, requirement 1).
    ///
    /// # Errors
    ///
    /// [`Error::PortInUse`] when another program holds the port, [`Error::Bind`] for any other
    /// socket failure, and [`Error::NotLoopback`] if the bound address is somehow not loopback.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "listen() registers with tokio's reactor, which exists only inside the runtime: async keeps \
                  every caller there"
    )]
    pub(crate) async fn bind(port: u16, registry: Arc<Registry>) -> Result<Self, Error> {
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
        tracing::info!(%local_addr, "local Galaxy service listening");
        Ok(Self {
            listener,
            local_addr,
            registry,
        })
    }

    /// Serves until `shutdown` completes, then closes the listener and every connection.
    #[expect(
        clippy::integer_division_remainder_used,
        reason = "the `%` is inside tokio::select!'s expansion"
    )]
    pub(crate) async fn run<F>(self, shutdown: F)
    where
        F: Future<Output = ()>,
    {
        let gate = Gate {
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
            unauthenticated: Arc::new(Semaphore::new(MAX_UNAUTHENTICATED)),
            refusals: Arc::new(Refusals::default()),
        };
        let mut connections = JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            while connections.try_join_next().is_some() {}
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                accepted = self.listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        if let Some(admission) = gate.admit(peer) {
                            connections.spawn(serve(stream, peer, self.local_addr, Arc::clone(&self.registry), admission));
                        }
                    }
                    Err(err) => {
                        tracing::warn!(%err, "accepting a Galaxy connection failed");
                        sleep(ACCEPT_BACKOFF).await;
                    }
                },
            }
        }
        connections.shutdown().await;
        let refused = gate.refusals.summary();
        tracing::info!(local_addr = %self.local_addr, %refused, "local Galaxy service stopped");
    }
}

/// What the accept loop checks before a connection gets a task: nothing here waits, so a flood
/// of connections costs the loop only the refusals.
struct Gate {
    slots: Arc<Semaphore>,
    unauthenticated: Arc<Semaphore>,
    refusals: Arc<Refusals>,
}

struct Admission {
    _slot: OwnedSemaphorePermit,
    /// Given back once the connection signs in.
    unauthenticated: OwnedSemaphorePermit,
    refusals: Arc<Refusals>,
}

impl Gate {
    fn admit(&self, peer: SocketAddr) -> Option<Admission> {
        let refuse = |kind: Refusal, cap: usize| {
            if let Some(total) = self.refusals.record(kind) {
                tracing::warn!(%peer, refusal = %kind, total, cap, "Galaxy connection refused");
            }
            None
        };
        // The listener is bound to 127.0.0.1, so this only fails if that ever changes.
        if !peer.ip().to_canonical().is_loopback() {
            return refuse(Refusal::NotLoopback, 0);
        }
        let Ok(slot) = Arc::clone(&self.slots).try_acquire_owned() else {
            return refuse(Refusal::TooManyConnections, MAX_CONNECTIONS);
        };
        let Ok(unauthenticated) = Arc::clone(&self.unauthenticated).try_acquire_owned() else {
            return refuse(Refusal::TooManyUnauthenticated, MAX_UNAUTHENTICATED);
        };
        Some(Admission {
            _slot: slot,
            unauthenticated,
            refusals: Arc::clone(&self.refusals),
        })
    }
}

/// Whether the peer may be served at all, from the owner of its socket (plan 1.5).
async fn same_user(peer: SocketAddr, local: SocketAddr, refusals: &Refusals) -> bool {
    let verdict = match spawn_blocking(move || peer::verdict(peer, local)).await {
        Ok(verdict) => verdict,
        Err(err) => {
            tracing::warn!(%peer, %err, "the Galaxy peer check did not finish");
            return false;
        }
    };
    match verdict {
        Verdict::SameUser => true,
        Verdict::OtherUser { peer_uid, own_uid } => {
            if let Some(total) = refusals.record(Refusal::OtherUser) {
                tracing::warn!(%peer, peer_uid, own_uid, total, "Galaxy connection refused: another user's program");
            }
            false
        }
        Verdict::Unknown(reason) if peer::ACCEPT_PEERS_OF_UNKNOWN_OWNER => {
            tracing::warn!(%peer, %reason, "Galaxy connection of unknown owner accepted, as this build is set to");
            true
        }
        Verdict::Unknown(reason) => {
            if let Some(total) = refusals.record(Refusal::OwnerUnknown) {
                tracing::warn!(%peer, %reason, total, "Galaxy connection refused: its owner cannot be told");
            }
            false
        }
    }
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the permit is the connection's unauthenticated slot, held until it signs in"
)]
async fn serve(stream: TcpStream, peer: SocketAddr, local: SocketAddr, registry: Arc<Registry>, admission: Admission) {
    let Admission {
        _slot,
        unauthenticated: sign_in_permit,
        refusals,
    } = admission;
    let sign_in_by = Instant::now().checked_add(SIGN_IN_DEADLINE);
    if !same_user(peer, local, &refusals).await {
        return;
    }
    let mut unauthenticated = Some(sign_in_permit);
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut connection = Connection::new(peer, registry, Arc::clone(&refusals));
    let mut requests = TokenBucket::new(REQUESTS_PER_SECOND, REQUEST_BURST, Instant::now());
    let mut rate_limited = 0_u64;
    let mut ended = "closed by the peer";
    loop {
        let next = read_frame::<Header, _>(&mut reader, MAX_PAYLOAD_BYTES);
        let read = match (unauthenticated.is_some(), sign_in_by) {
            (true, Some(deadline)) => match timeout_at(deadline, next).await {
                Ok(read) => read,
                Err(_elapsed) => {
                    if let Some(total) = refusals.record(Refusal::SignInTimeout) {
                        tracing::warn!(%peer, total, deadline_secs = SIGN_IN_DEADLINE.as_secs(), "closing a Galaxy connection that did not sign in");
                    }
                    ended = "no sign-in in time";
                    break;
                }
            },
            _ => next.await,
        };
        let frame = match read {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(err) => {
                if let Some(total) = refusals.record(Refusal::BadFrame) {
                    tracing::warn!(%peer, %err, total, "closing a Galaxy connection on a bad frame");
                }
                ended = "bad frame";
                break;
            }
        };
        let (header, payload, pause) = match requests.take(Instant::now()) {
            Ok(()) => {
                let (header, payload) = connection.handle(&frame).await;
                (header, payload, None)
            }
            Err(wait) => {
                rate_limited = rate_limited.saturating_add(1);
                let total = refusals.record(Refusal::RateLimited);
                if rate_limited == 1 {
                    tracing::warn!(%peer, ?total, rate = REQUESTS_PER_SECOND, "Galaxy connection over its request rate, slowed down");
                }
                let (header, payload) = connection.rate_limited(&frame.header, wait);
                (header, payload, Some(wait))
            }
        };
        if let Err(err) = write_frame(&mut writer, header, &payload).await {
            tracing::warn!(%peer, %err, "writing a Galaxy response failed");
            ended = "write failed";
            break;
        }
        if connection.is_signed_in() {
            unauthenticated = None;
        }
        // Reading nothing more until the bucket refills is what slows a flooding client down,
        // and only its own task waits.
        if let Some(wait) = pause {
            sleep(wait).await;
        }
    }
    let answered = summary(connection.counts());
    tracing::info!(%peer, ended, requests = %answered, rate_limited, "Galaxy connection ended");
}

/// `sort/type=code xcount` per request kind, the per-connection record plan 2 asks for.
fn summary(counts: &BTreeMap<(u32, u32, i32), u32>) -> String {
    let mut text = String::new();
    for (&(sort, kind, code), count) in counts {
        let separator = if text.is_empty() { "" } else { " " };
        let written = if sort == OTHER_KINDS {
            write!(text, "{separator}other={code}x{count}")
        } else {
            write!(text, "{separator}{sort}/{kind}={code}x{count}")
        };
        if written.is_err() {
            break;
        }
    }
    text
}
