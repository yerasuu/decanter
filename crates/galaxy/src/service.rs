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
use crate::host::Registry;
use crate::proto::pb::Header;

/// The port every Galaxy SDK build dials; the SDK offers no way to choose another (plan 1.2).
pub const DEFAULT_PORT: u16 = 9977;
/// The plan's cap (2.4): a few games at once, each with a couple of SDK connections.
const MAX_CONNECTIONS: usize = 16;
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
                            connections.spawn(serve(stream, peer, Arc::clone(&self.registry), permit));
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

async fn serve(stream: TcpStream, peer: SocketAddr, registry: Arc<Registry>, _slot: OwnedSemaphorePermit) {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut connection = Connection::new(peer, registry);
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
