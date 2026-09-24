//! State shared between web servers through Valkey (or Redis): rate limit
//! counters and cached search counts. See `cache.backend`.
//!
//! The connection is made on first use and remade after failures, with a
//! short timeout. While Valkey is unreachable, calls fail fast and callers
//! fall back to working without it, so an outage slows nothing down and
//! takes nothing offline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{AsyncCommands, Client, RedisError, RedisResult, Script};
use tokio::sync::Mutex;

/// How long connecting or a command may take.
const TIMEOUT: Duration = Duration::from_millis(500);
/// After a failed connection attempt, calls fail at once for this long.
const RETRY_AFTER: Duration = Duration::from_secs(10);

/// GCRA, the algorithm the in-memory limiter uses: each key stores the
/// "theoretical arrival time" of the next request. `ARGV[1]` is the
/// period between requests and `ARGV[2]` the burst, both in milliseconds
/// and requests. Returns 0 when allowed, else milliseconds to wait. Uses
/// the server's clock, so every web server agrees.
const GCRA: &str = r"
local t = redis.call('TIME')
local now = tonumber(t[1]) * 1000 + math.floor(tonumber(t[2]) / 1000)
local period = tonumber(ARGV[1])
local burst = tonumber(ARGV[2])
local tat = tonumber(redis.call('GET', KEYS[1]) or now)
if tat < now then tat = now end
local allow_at = tat + period - burst * period
if now < allow_at then return allow_at - now end
local new_tat = tat + period
redis.call('SET', KEYS[1], new_tat, 'PX', new_tat - now)
return 0
";

#[derive(Clone)]
pub struct Valkey {
    /// Starts every key, so sites can share a server.
    prefix: Arc<str>,
    client: Client,
    state: Arc<Mutex<Connection>>,
    gcra: Arc<Script>,
}

enum Connection {
    None,
    Up(ConnectionManager),
    Down(Instant),
}

fn unavailable(message: &str) -> RedisError {
    RedisError::from((
        redis::ErrorKind::Io,
        "Valkey unavailable",
        message.to_owned(),
    ))
}

impl Valkey {
    /// Checks `url`; connects on first use. Keys are stored as
    /// `<prefix>:<key>`.
    pub fn new(url: &str, prefix: &str) -> RedisResult<Self> {
        Ok(Self {
            prefix: prefix.into(),
            client: Client::open(url)?,
            state: Arc::new(Mutex::new(Connection::None)),
            gcra: Arc::new(Script::new(GCRA)),
        })
    }

    async fn connection(&self) -> RedisResult<ConnectionManager> {
        let mut state = self.state.lock().await;
        match &*state {
            Connection::Up(connection) => return Ok(connection.clone()),
            Connection::Down(since) if since.elapsed() < RETRY_AFTER => {
                return Err(unavailable("waiting before reconnecting"));
            }
            _ => {}
        }
        let config = ConnectionManagerConfig::new()
            .set_connection_timeout(Some(TIMEOUT))
            .set_response_timeout(Some(TIMEOUT))
            .set_number_of_retries(1);
        match tokio::time::timeout(
            TIMEOUT * 2,
            ConnectionManager::new_with_config(self.client.clone(), config),
        )
        .await
        {
            Ok(Ok(connection)) => {
                *state = Connection::Up(connection.clone());
                Ok(connection)
            }
            Ok(Err(error)) => {
                *state = Connection::Down(Instant::now());
                Err(error)
            }
            Err(_) => {
                *state = Connection::Down(Instant::now());
                Err(unavailable("connecting timed out"))
            }
        }
    }

    /// Remembers that `result` failed, so calls fail fast for a while
    /// instead of each waiting out the timeout.
    async fn note<T>(&self, result: RedisResult<T>) -> RedisResult<T> {
        if result.is_err() {
            *self.state.lock().await = Connection::Down(Instant::now());
        }
        result
    }

    /// Counts a request against `key`'s limit of `burst` at once and one
    /// more every `period`: `None` if allowed, else how long to wait.
    pub async fn gcra(
        &self,
        key: &str,
        burst: u32,
        period: Duration,
    ) -> RedisResult<Option<Duration>> {
        let mut connection = self.connection().await?;
        let wait: RedisResult<u64> = self
            .gcra
            .key(self.key(key))
            .arg(period.as_millis() as u64)
            .arg(burst)
            .invoke_async(&mut connection)
            .await;
        let wait = self.note(wait).await?;
        Ok((wait > 0).then(|| Duration::from_millis(wait)))
    }

    fn key(&self, key: &str) -> String {
        format!("{}:{key}", self.prefix)
    }

    pub async fn get(&self, key: &str) -> RedisResult<Option<String>> {
        let mut connection = self.connection().await?;
        let value = connection.get(self.key(key)).await;
        self.note(value).await
    }

    pub async fn set(&self, key: &str, value: &str, ttl: Duration) -> RedisResult<()> {
        let mut connection = self.connection().await?;
        let done = connection
            .set_ex(self.key(key), value, ttl.as_secs().max(1))
            .await;
        self.note(done).await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::net::IpAddr;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A real Valkey from `TEST_VALKEY_URL`, if set.
    pub async fn valkey() -> Option<Valkey> {
        let url = std::env::var("TEST_VALKEY_URL").ok()?;
        // Valkey keeps state between runs: each client gets its own keys.
        Some(Valkey::new(&url, &unique("moekura-test")).expect("TEST_VALKEY_URL is a redis:// URL"))
    }

    /// A client for a port nothing listens on.
    pub async fn unreachable() -> Option<Valkey> {
        Some(Valkey::new("redis://127.0.0.1:1", "moekura").unwrap())
    }

    /// A number no other call, in this run or another, gets: Valkey keeps
    /// state across runs.
    fn fresh() -> u128 {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        (nanos << 32) | u128::from(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    pub fn unique(prefix: &str) -> String {
        format!("{prefix}-{}", fresh())
    }

    /// An address in the IPv6 documentation range.
    pub fn unique_ip() -> IpAddr {
        let bits = (0x2001_0db8_u128 << 96) | (fresh() & ((1 << 96) - 1));
        IpAddr::from(bits.to_be_bytes())
    }

    #[tokio::test]
    async fn fails_fast_while_down() {
        let valkey = unreachable().await.unwrap();
        assert!(valkey.get("x").await.is_err());
        let started = Instant::now();
        assert!(valkey.get("x").await.is_err());
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "no reconnect storm"
        );
    }

    #[tokio::test]
    async fn stores_values_when_configured() {
        let Some(valkey) = valkey().await else {
            eprintln!("skipping: TEST_VALKEY_URL not set");
            return;
        };
        let key = unique("moekura:test");
        valkey
            .set(&key, "42", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(valkey.get(&key).await.unwrap().as_deref(), Some("42"));
        // GCRA: a burst of 2, then waiting about a period.
        let key = unique("gcra");
        let period = Duration::from_secs(60);
        assert_eq!(valkey.gcra(&key, 2, period).await.unwrap(), None);
        assert_eq!(valkey.gcra(&key, 2, period).await.unwrap(), None);
        let wait = valkey.gcra(&key, 2, period).await.unwrap().unwrap();
        assert!(wait > Duration::from_secs(55) && wait <= period, "{wait:?}");
    }
}
