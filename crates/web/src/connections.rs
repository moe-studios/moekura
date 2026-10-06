//! Accepting and serving the site's connections. `axum::serve` sets no
//! limits: a client could open connections and send nothing, or its
//! headers a byte at a time, or stop reading what it asked for, and hold
//! them until the process ran out of file descriptors. Here at most
//! [`Limits::max`] are served at once. One that goes about
//! [`Limits::idle`] without sending a request is closed, and if it then
//! takes none of its response for [`Limits::send_timeout`], it's cut off.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder;
use hyper_util::service::TowerToHyperService;
use moekura_core::config::ConnectionConfig;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tower::{Layer, ServiceExt};

/// How many connections are served at once, how long one may go without
/// sending a request, and how long a closing one may take none of its
/// response.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max: u32,
    pub idle: Duration,
    pub send_timeout: Duration,
}

impl From<&ConnectionConfig> for Limits {
    fn from(config: &ConnectionConfig) -> Self {
        Self {
            max: config.max.max(1),
            idle: Duration::from_secs(config.idle_timeout_secs.max(1)),
            send_timeout: Duration::from_secs(config.send_timeout_secs.max(1)),
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
    // No keep-alive pings: writing them would count as the client taking
    // something, while peers that vanished, or ignore the server, take
    // nothing and are cut off. The CONNECT protocol is for websockets over
    // HTTP/2, as axum::serve has it.
    http.http2().enable_connect_protocol();
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
            limits,
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

/// Serves one connection until it closes. It's asked to close (after the
/// responses in progress, if any) when it has sent no request for
/// `limits.idle`, or when the server stops. From then on, while no
/// handler is still working on a response, it's cut off once the client
/// has taken nothing for `limits.send_timeout`: hyper never gives up on a
/// write the client doesn't read, nor on an HTTP/2 response whose window
/// the client never opens.
async fn serve_connection(
    http: Arc<Builder<TokioExecutor>>,
    stream: TcpStream,
    service: axum::middleware::AddExtension<Router, ConnectInfo<SocketAddr>>,
    limits: Limits,
    mut stopping: watch::Receiver<bool>,
    _slot: OwnedSemaphorePermit,
) {
    let activity = Arc::new(Activity::new());
    let counted = activity.clone();
    let service = ServiceExt::<Request<Body>>::map_future(service, move |response| {
        let handling = Handling::start(counted.clone());
        async move {
            let response = response.await;
            drop(handling);
            response
        }
    })
    // hyper's request bodies, as axum::serve passes them on.
    .map_request(|request: Request<_>| request.map(Body::new));
    let socket = Socket {
        stream,
        activity: activity.clone(),
    };
    let connection = http
        .serve_connection_with_upgrades(TokioIo::new(socket), TowerToHyperService::new(service));
    let mut connection = pin!(connection);
    // hyper's header timeout starts only once it knows the protocol, and
    // HTTP/2 has none: a connection that has started no request since the
    // last check is idle. That also covers one that never sent a byte.
    let mut checks = tokio::time::interval_at(Instant::now() + limits.idle, limits.idle);
    checks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seen = 0;
    let mut closing = false;
    // Once closing: when the client may next have taken nothing for too long.
    let mut stalled = pin!(tokio::time::sleep(limits.send_timeout));
    loop {
        let close = tokio::select! {
            served = connection.as_mut() => {
                if let Err(error) = served {
                    tracing::trace!(%error, "connection closed");
                }
                return;
            }
            _ = checks.tick(), if !closing => {
                let started = activity.requests.load(Ordering::Relaxed);
                let idle = started == seen;
                seen = started;
                idle
            }
            _ = stopping.wait_for(|stop| *stop), if !closing => true,
            () = &mut stalled, if closing => {
                let quiet = activity.quiet_for();
                if quiet >= limits.send_timeout {
                    tracing::debug!("cut off a connection whose client took none of its response");
                    activity.cut_off.store(true, Ordering::Relaxed);
                    return;
                }
                stalled.as_mut().reset(Instant::now() + (limits.send_timeout - quiet));
                false
            }
        };
        if close {
            connection.as_mut().graceful_shutdown();
            closing = true;
            stalled.as_mut().reset(Instant::now() + limits.send_timeout);
        }
    }
}

/// What a connection has been doing, as its checks need to know.
struct Activity {
    opened: Instant,
    /// Requests started.
    requests: AtomicU64,
    /// Requests whose handler hasn't answered yet.
    handling: AtomicU64,
    /// When the socket last took something to send the client, in
    /// milliseconds after `opened`.
    sent: AtomicU64,
    /// Whether the connection was cut off, rather than closed.
    cut_off: AtomicBool,
}

impl Activity {
    fn new() -> Self {
        Self {
            opened: Instant::now(),
            requests: AtomicU64::new(0),
            handling: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            cut_off: AtomicBool::new(false),
        }
    }

    /// How long the client has taken nothing, while no handler is working
    /// out what to send it.
    fn quiet_for(&self) -> Duration {
        if self.handling.load(Ordering::Relaxed) > 0 {
            return Duration::ZERO;
        }
        let sent = Duration::from_millis(self.sent.load(Ordering::Relaxed));
        self.opened.elapsed().saturating_sub(sent)
    }
}

/// A request from its start until its handler answers, or is dropped.
struct Handling(Arc<Activity>);

impl Handling {
    fn start(activity: Arc<Activity>) -> Self {
        activity.requests.fetch_add(1, Ordering::Relaxed);
        activity.handling.fetch_add(1, Ordering::Relaxed);
        Self(activity)
    }
}

impl Drop for Handling {
    fn drop(&mut self) {
        self.0.handling.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A connection's socket, noting when it takes something to send.
struct Socket {
    stream: TcpStream,
    activity: Arc<Activity>,
}

impl Socket {
    fn note(&self, written: &Poll<io::Result<usize>>) {
        if let Poll::Ready(Ok(1..)) = written {
            let at = self.activity.opened.elapsed().as_millis();
            self.activity
                .sent
                .store(u64::try_from(at).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    }
}

impl AsyncRead for Socket {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Socket {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let written = Pin::new(&mut self.stream).poll_write(cx, buf);
        self.note(&written);
        written
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let written = Pin::new(&mut self.stream).poll_write_vectored(cx, bufs);
        self.note(&written);
        written
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // Reset rather than close: a closed socket keeps what the client
        // didn't take in the kernel, still trying to send it.
        if self.activity.cut_off.load(Ordering::Relaxed) {
            let _ = self.stream.set_zero_linger();
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
    use axum::http::Request;
    use axum::routing::get;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpSocket, TcpStream};
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use super::{Limits, serve};

    struct Server {
        addr: SocketAddr,
        stop: oneshot::Sender<()>,
        served: JoinHandle<std::io::Result<()>>,
    }

    /// The size of `/big`: more than the socket buffers on both ends and
    /// hyper's hold.
    const BIG: usize = 16 << 20;

    /// A server answering `/` with the client's address, `/slow` after a
    /// while, and `/big` with [`BIG`] bytes.
    async fn start(max: u32, idle_ms: u64, send_timeout_ms: u64) -> Server {
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
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        "done"
                    }),
                )
                .route("/big", get(|| async { vec![0u8; BIG] }));
        let (stop, stopped) = oneshot::channel();
        let limits = Limits {
            max,
            idle: Duration::from_millis(idle_ms),
            send_timeout: Duration::from_millis(send_timeout_ms),
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

    /// A connection, with as small a receive buffer as it gets, that has
    /// asked for `/big` and read none of it yet.
    async fn ask_for_big(addr: SocketAddr) -> TcpStream {
        let socket = TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        let mut stream = socket.connect(addr).await.unwrap();
        stream
            .write_all(b"GET /big HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        stream
    }

    #[tokio::test]
    async fn handlers_see_the_peer_address() {
        let server = start(8, 1000, 1000).await;
        let response = get_once(server.addr, "/").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("\r\n\r\n127.0.0.1"), "{response}");
    }

    #[tokio::test]
    async fn speaks_http2() {
        moekura_storage::install_crypto_provider();
        let server = start(8, 1000, 1000).await;
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
        let server = start(8, 200, 300).await;
        let mut silent = TcpStream::connect(server.addr).await.unwrap();
        read_until_closed(&mut silent, Duration::from_secs(2)).await;
    }

    #[tokio::test]
    async fn slow_headers_are_cut_off() {
        let server = start(8, 200, 300).await;
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
        let server = start(8, 200, 300).await;
        // Kept alive after a response...
        let mut kept = TcpStream::connect(server.addr).await.unwrap();
        kept.write_all(b"GET / HTTP/1.1\r\nHost: test\r\n\r\n")
            .await
            .unwrap();
        let response = read_until_closed(&mut kept, Duration::from_secs(2)).await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        // ...and HTTP/2 from a client that starts no request (nor answers
        // the server's goodbye).
        let mut idle = TcpStream::connect(server.addr).await.unwrap();
        idle.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\0\0\0\x04\0\0\0\0\0")
            .await
            .unwrap();
        read_until_closed(&mut idle, Duration::from_secs(3)).await;
    }

    #[tokio::test]
    async fn connections_past_the_limit_wait_their_turn() {
        let server = start(1, 5000, 5000).await;
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
    async fn clients_that_stop_reading_are_cut_off() {
        let server = start(1, 100, 300).await;
        let _stalled = ask_for_big(server.addr).await;
        // The only slot, which it holds, comes free.
        let response = get_once(server.addr, "/").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }

    #[tokio::test]
    async fn http2_clients_that_open_no_window_are_cut_off() {
        let server = start(1, 100, 300).await;
        // It answers pings and the server's goodbye, but lets no data
        // through: even the 9 bytes of `/` wait for it.
        let tcp = TcpStream::connect(server.addr).await.unwrap();
        let (client, connection) = h2::client::Builder::new()
            .initial_window_size(0)
            .handshake::<_, &'static [u8]>(tcp)
            .await
            .unwrap();
        let connection = tokio::spawn(connection);
        let mut client = client.ready().await.unwrap();
        let request = Request::get("http://test/").body(()).unwrap();
        let (response, _) = client.send_request(request, true).unwrap();
        let _response = response.await.unwrap();

        let response = get_once(server.addr, "/").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let _ = tokio::time::timeout(Duration::from_secs(2), connection)
            .await
            .expect("the server ends the connection")
            .unwrap();
    }

    #[tokio::test]
    async fn clients_that_keep_reading_are_not_cut_off() {
        let server = start(8, 100, 1000).await;
        let mut stream = ask_for_big(server.addr).await;
        // A burst at a time, with pauses, for longer than the send timeout
        // in all.
        let mut buf = vec![0; 64 * 1024];
        let mut received = 0;
        'reading: loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let burst = received + (2 << 20);
            while received < burst {
                let n = stream.read(&mut buf).await.expect("not cut off");
                if n == 0 {
                    break 'reading;
                }
                received += n;
            }
        }
        assert!(received > BIG, "{received}");
    }

    #[tokio::test]
    async fn slow_handlers_are_not_cut_off() {
        // Closing after about 200 ms, while the handler takes 500.
        let server = start(8, 100, 100).await;
        let response = get_once(server.addr, "/slow").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.ends_with("done"), "{response}");
    }

    #[tokio::test]
    async fn stopping_finishes_requests_in_flight() {
        let server = start(8, 1000, 1000).await;
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
