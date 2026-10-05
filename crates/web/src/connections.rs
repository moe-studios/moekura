//! Accepting and serving the site's connections. `axum::serve` sets no
//! limits: a client could open connections and send nothing, or its
//! headers a byte at a time, and hold them until the process ran out of
//! file descriptors. Here at most [`Limits::max`] are served at once, and
//! one that goes about [`Limits::idle`] without sending a request is
//! closed.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::Request;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use moekura_core::config::ConnectionConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::MissedTickBehavior;
use tower::{Layer, ServiceExt};

/// How many connections are served at once, and how long one may go
/// without sending a request.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max: u32,
    pub idle: Duration,
}

impl From<&ConnectionConfig> for Limits {
    fn from(config: &ConnectionConfig) -> Self {
        Self {
            max: config.max.max(1),
            idle: Duration::from_secs(config.idle_timeout_secs.max(1)),
        }
    }
}

/// How often running out of connections is logged, at most.
const FULL_WARNING_EVERY: Duration = Duration::from_secs(60);

/// Serves `router` on `listener` until `shutdown` resolves, then lets
/// in-flight requests finish. Handlers see the peer's address as
/// `ConnectInfo<SocketAddr>`.
pub(crate) async fn serve(
    listener: TcpListener,
    router: Router,
    limits: Limits,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let mut http = Builder::new(TokioExecutor::new());
    // hyper only times out reading headers, which also closes idle
    // keep-alive connections, when it has a timer.
    http.http1()
        .timer(TokioTimer::new())
        .header_read_timeout(limits.idle);
    // Pings find peers that vanished, or ignore the server, in the middle
    // of a response or of closing. The CONNECT protocol is for websockets
    // over HTTP/2, as axum::serve has it.
    http.http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(limits.idle)
        .keep_alive_timeout(limits.idle)
        .enable_connect_protocol();
    let http = Arc::new(http);
    let slots = Arc::new(Semaphore::new(limits.max as usize));
    let (stop, stopping) = watch::channel(false);
    let mut shutdown = pin!(shutdown);
    let mut warned: Option<Instant> = None;
    loop {
        // When every slot is taken, new connections wait in the listen
        // backlog rather than being refused.
        let slot = match slots.clone().try_acquire_owned() {
            Ok(slot) => slot,
            Err(_) => {
                if warned.is_none_or(|at| at.elapsed() >= FULL_WARNING_EVERY) {
                    tracing::warn!(
                        max = limits.max,
                        "every connection is taken; new ones wait (server.connections.max)"
                    );
                    warned = Some(Instant::now());
                }
                tokio::select! {
                    slot = slots.clone().acquire_owned() => {
                        slot.expect("the semaphore is never closed")
                    }
                    () = &mut shutdown => break,
                }
            }
        };
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            () = &mut shutdown => break,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                accept_failed(error).await;
                continue;
            }
        };
        let service = axum::Extension(ConnectInfo(peer)).layer(router.clone());
        tokio::spawn(serve_connection(
            http.clone(),
            stream,
            service,
            limits.idle,
            stopping.clone(),
            slot,
        ));
    }
    drop(listener);
    let _ = stop.send(true);
    // Each connection holds its slot until it closes.
    let _ = slots.acquire_many(limits.max).await;
    Ok(())
}

/// Serves one connection until it closes, asking it to close (after the
/// response in progress, if any) when it has sent no request for `idle`,
/// or when the server stops.
async fn serve_connection(
    http: Arc<Builder<TokioExecutor>>,
    stream: TcpStream,
    service: axum::middleware::AddExtension<Router, ConnectInfo<SocketAddr>>,
    idle: Duration,
    mut stopping: watch::Receiver<bool>,
    _slot: OwnedSemaphorePermit,
) {
    let requests = Arc::new(AtomicU64::new(0));
    let counted = requests.clone();
    let service = service.map_request(move |request: Request<_>| {
        counted.fetch_add(1, Ordering::Relaxed);
        request
    });
    let connection = http
        .serve_connection_with_upgrades(TokioIo::new(stream), TowerToHyperService::new(service));
    let mut connection = pin!(connection);
    // hyper's header timeout starts only once it knows the protocol, and
    // HTTP/2 has none: a connection that has started no request since the
    // last check is idle. That also covers one that never sent a byte.
    let mut checks = tokio::time::interval_at(tokio::time::Instant::now() + idle, idle);
    checks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seen = 0;
    let mut closing = false;
    loop {
        tokio::select! {
            served = connection.as_mut() => {
                if let Err(error) = served {
                    tracing::trace!(%error, "connection closed");
                }
                break;
            }
            _ = checks.tick(), if !closing => {
                let started = requests.load(Ordering::Relaxed);
                if started == seen {
                    connection.as_mut().graceful_shutdown();
                    closing = true;
                }
                seen = started;
            }
            _ = stopping.wait_for(|stop| *stop), if !closing => {
                connection.as_mut().graceful_shutdown();
                closing = true;
            }
        }
    }
}

/// As `axum::serve` does: a connection reset before it was accepted is
/// no matter, while anything else (such as running out of file
/// descriptors) is logged, and accepting pauses to let connections close.
async fn accept_failed(error: io::Error) {
    if matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    ) {
        return;
    }
    tracing::error!(%error, "could not accept a connection");
    tokio::time::sleep(Duration::from_secs(1)).await;
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use axum::Router;
    use axum::extract::ConnectInfo;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use super::{Limits, serve};

    struct Server {
        addr: SocketAddr,
        stop: oneshot::Sender<()>,
        served: JoinHandle<std::io::Result<()>>,
    }

    /// A server answering `/` with the client's address and `/slow`
    /// after a while.
    async fn start(max: u32, idle_ms: u64) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router =
            Router::new()
                .route(
                    "/",
                    get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
                        peer.ip().to_string()
                    }),
                )
                .route(
                    "/slow",
                    get(|| async {
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        "done"
                    }),
                );
        let (stop, stopped) = oneshot::channel();
        let limits = Limits {
            max,
            idle: Duration::from_millis(idle_ms),
        };
        let served = tokio::spawn(serve(listener, router, limits, async {
            let _ = stopped.await;
        }));
        Server { addr, stop, served }
    }

    /// What arrives until the server closes `stream`, which must be
    /// within `within`.
    async fn read_until_closed(stream: &mut TcpStream, within: Duration) -> String {
        let mut received = Vec::new();
        tokio::time::timeout(within, async {
            let mut buf = [0; 1024];
            while let Ok(n @ 1..) = stream.read(&mut buf).await {
                received.extend_from_slice(&buf[..n]);
            }
        })
        .await
        .expect("the server closes the connection");
        String::from_utf8_lossy(&received).into_owned()
    }

    /// The response to one request on a connection of its own.
    async fn get_once(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        read_until_closed(&mut stream, Duration::from_secs(5)).await
    }

    #[tokio::test]
    async fn handlers_see_the_peer_address() {
        let server = start(8, 1000).await;
        let response = get_once(server.addr, "/").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("\r\n\r\n127.0.0.1"), "{response}");
    }

    #[tokio::test]
    async fn speaks_http2() {
        moekura_storage::install_crypto_provider();
        let server = start(8, 1000).await;
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .build()
            .unwrap();
        let response = client
            .get(format!("http://{}/", server.addr))
            .send()
            .await
            .unwrap();
        assert_eq!(response.version(), reqwest::Version::HTTP_2);
        assert_eq!(response.text().await.unwrap(), "127.0.0.1");
    }

    #[tokio::test]
    async fn connections_that_send_nothing_are_closed() {
        let server = start(8, 200).await;
        let mut silent = TcpStream::connect(server.addr).await.unwrap();
        read_until_closed(&mut silent, Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn slow_headers_are_cut_off() {
        let server = start(8, 200).await;
        // Part of a request, and part of the HTTP/2 preface, which hyper
        // reads before its own header timeout starts.
        for partial in ["GET / HTTP/1.1\r\nHost: te", "PRI * HTTP/2.0\r\n"] {
            let mut slow = TcpStream::connect(server.addr).await.unwrap();
            slow.write_all(partial.as_bytes()).await.unwrap();
            let response = read_until_closed(&mut slow, Duration::from_secs(2)).await;
            assert!(!response.contains("200 OK"), "{partial:?}: {response}");
        }
    }

    #[tokio::test]
    async fn idle_connections_are_closed() {
        let server = start(8, 200).await;
        // Kept alive after a response...
        let mut kept = TcpStream::connect(server.addr).await.unwrap();
        kept.write_all(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
        let response = read_until_closed(&mut kept, Duration::from_secs(2)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        // ...and HTTP/2 from a client that starts no request (nor answers
        // pings).
        let mut idle = TcpStream::connect(server.addr).await.unwrap();
        idle.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\0\0\0\x04\0\0\0\0\0")
            .await
            .unwrap();
        read_until_closed(&mut idle, Duration::from_secs(3)).await;
    }

    #[tokio::test]
    async fn connections_past_the_limit_wait_their_turn() {
        let server = start(1, 5000).await;
        let first = TcpStream::connect(server.addr).await.unwrap();
        let mut second = TcpStream::connect(server.addr).await.unwrap();
        second
            .write_all(b"GET / HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0; 64];
        let early = tokio::time::timeout(Duration::from_millis(300), second.read(&mut buf)).await;
        assert!(early.is_err(), "served while the only slot was taken");
        drop(first);
        let response = read_until_closed(&mut second, Duration::from_secs(2)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }

    #[tokio::test]
    async fn stopping_finishes_requests_in_flight() {
        let server = start(8, 1000).await;
        let slow = tokio::spawn(get_once(server.addr, "/slow"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        server.stop.send(()).unwrap();
        let response = slow.await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("done"), "{response}");
        tokio::time::timeout(Duration::from_secs(2), server.served)
            .await
            .expect("stops once the request is done")
            .unwrap()
            .unwrap();
        assert!(TcpStream::connect(server.addr).await.is_err());
    }
}
