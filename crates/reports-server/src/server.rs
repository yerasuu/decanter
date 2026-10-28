//! The accept loop: a connection cap, a header-read timeout and a bounded shutdown, none of which plain
//! `axum::serve` offers. Without them idle or half-open sockets are accepted until the process runs out of
//! file descriptors.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::ConnectInfo;
use hyper::Request;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::server::graceful::GracefulShutdown;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower::ServiceExt;

use crate::config::Config;

const ACCEPT_RETRY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_connections: usize,
    pub header_timeout: Duration,
    pub shutdown_grace: Duration,
}

impl Limits {
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self {
            max_connections: config.max_connections as usize,
            header_timeout: Duration::from_secs(config.header_timeout_secs.into()),
            shutdown_grace: Duration::from_secs(config.shutdown_grace_secs.into()),
        }
    }
}

/// Serves until `shutdown` resolves, then gives open connections `shutdown_grace` to finish.
///
/// The header timer in hyper starts whenever the server waits for a request head, so it bounds slow headers and
/// idle keep-alive connections alike. A connection over the cap is closed at once instead of queued: a flood
/// then costs a `close` per attempt, not a held descriptor.
pub async fn run(listener: TcpListener, app: Router, limits: Limits, shutdown: impl Future<Output = ()>) {
    let permits = Arc::new(Semaphore::new(limits.max_connections));
    let graceful = GracefulShutdown::new();
    let mut http = http1::Builder::new();
    http.timer(TokioTimer::new()).header_read_timeout(limits.header_timeout);
    let mut shutdown = std::pin::pin!(shutdown);
    let mut refused = 0_u64;
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::warn!(%error, "accept failed");
                    tokio::time::sleep(ACCEPT_RETRY).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            refused += 1;
            if refused % 100 == 1 {
                tracing::warn!(
                    refused,
                    max = limits.max_connections,
                    "connection cap reached: connection closed"
                );
            }
            continue;
        };
        let service = service_fn({
            let app = app.clone();
            move |mut request: Request<Incoming>| {
                request.extensions_mut().insert(ConnectInfo::<SocketAddr>(peer));
                app.clone().oneshot(request)
            }
        });
        let connection = graceful.watch(http.serve_connection(TokioIo::new(stream), service));
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::debug!(%error, "connection closed with an error");
            }
            drop(permit);
        });
    }
    drop(listener);
    if tokio::time::timeout(limits.shutdown_grace, graceful.shutdown())
        .await
        .is_err()
    {
        tracing::warn!(
            open = limits.max_connections - permits.available_permits(),
            "shutdown grace elapsed: open connections dropped"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::time::Instant;

    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::oneshot;

    use super::*;

    type Test = Result<(), Box<dyn Error>>;

    async fn start(
        limits: Limits,
    ) -> Result<(SocketAddr, oneshot::Sender<()>, tokio::task::JoinHandle<()>), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let (stop, stopped) = oneshot::channel::<()>();
        let app = Router::new().route("/healthz", get(|| async { "ok" }));
        let task = tokio::spawn(async move {
            run(listener, app, limits, async {
                let _ = stopped.await;
            })
            .await;
        });
        Ok((addr, stop, task))
    }

    fn limits(max_connections: usize, header_secs: u64, grace_secs: u64) -> Limits {
        Limits {
            max_connections,
            header_timeout: Duration::from_secs(header_secs),
            shutdown_grace: Duration::from_secs(grace_secs),
        }
    }

    async fn closed_within(stream: &mut TcpStream, wait: Duration) -> bool {
        let mut buffer = [0_u8; 64];
        matches!(
            tokio::time::timeout(wait, stream.read(&mut buffer)).await,
            Ok(Ok(0) | Err(_))
        )
    }

    #[tokio::test]
    async fn serves_a_request_with_the_peer_address() -> Test {
        let (addr, _stop, _task) = start(limits(4, 5, 1)).await?;
        let mut stream = TcpStream::connect(addr).await?;
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n")
            .await?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer).await?;
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        Ok(())
    }

    #[tokio::test]
    async fn partial_head_and_idle_connection_are_closed_at_the_timeout() -> Test {
        let (addr, _stop, _task) = start(limits(8, 1, 1)).await?;
        let mut partial = TcpStream::connect(addr).await?;
        partial.write_all(b"GET /healthz HTT").await?;
        let mut idle = TcpStream::connect(addr).await?;
        let began = Instant::now();
        assert!(closed_within(&mut partial, Duration::from_secs(4)).await);
        assert!(closed_within(&mut idle, Duration::from_secs(4)).await);
        assert!(began.elapsed() < Duration::from_secs(4));
        Ok(())
    }

    #[tokio::test]
    async fn connections_over_the_cap_are_closed_at_once() -> Test {
        let (addr, _stop, _task) = start(limits(2, 30, 1)).await?;
        let mut kept = vec![TcpStream::connect(addr).await?, TcpStream::connect(addr).await?];
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut over = TcpStream::connect(addr).await?;
        assert!(closed_within(&mut over, Duration::from_secs(2)).await);
        for stream in &mut kept {
            assert!(!closed_within(stream, Duration::from_millis(300)).await);
        }
        drop(kept);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut again = TcpStream::connect(addr).await?;
        assert!(!closed_within(&mut again, Duration::from_millis(300)).await);
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_does_not_wait_for_an_open_connection() -> Test {
        let (addr, stop, task) = start(limits(4, 30, 1)).await?;
        let mut idle = TcpStream::connect(addr).await?;
        idle.write_all(b"GET /healthz HTT").await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let began = Instant::now();
        stop.send(()).map_err(|()| "server gone")?;
        tokio::time::timeout(Duration::from_secs(5), task).await??;
        assert!(began.elapsed() < Duration::from_secs(4));
        Ok(())
    }
}
