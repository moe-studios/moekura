//! The `webhooks.deliver` job: sending one delivery, signed, to its
//! webhook; and `webhooks.prune`, forgetting old deliveries.

use std::fmt::Write as _;
use std::time::Duration;

use moekura_core::jobs::{DeliverWebhook, PruneWebhookDeliveries};
use moekura_core::webhooks::{Event, Format, discord, signature};
use moekura_db::webhooks;
use sqlx::PgPool;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{JobError, Registry};

/// How long deliveries are kept, in days.
const KEEP_DAYS: i32 = 30;

/// How much of a response is kept.
const RESPONSE_KEPT: usize = 1000;

/// How much of a response is read: plenty for Discord's answer, the
/// message it made. The rest is never downloaded.
const RESPONSE_READ: usize = 64 * 1024;

/// The longest `Retry-After` honoured, in seconds; longer waits are cut
/// to this.
const MAX_RETRY_AFTER: f64 = 3600.0;

/// Deliveries in a row that couldn't reach a receiver (or timed out)
/// before its webhook is paused.
const PAUSE_AFTER: i32 = 5;

/// How long a paused webhook's deliveries wait (retried as usual, without
/// sending) before the next one tries the receiver again.
const PAUSE: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub struct WebhookJobs {
    pub db: PgPool,
    pub client: reqwest::Client,
    pub allow_private: bool,
    /// Deliveries sent at once, across all workers.
    pub max_concurrent: u32,
}

impl WebhookJobs {
    /// A sender whose connections only go to public addresses (unless
    /// `allow_private`), following no redirects, one delivery at a time.
    pub fn new(db: PgPool, timeout: Duration, allow_private: bool) -> Self {
        let client = moekura_net::client(timeout, allow_private, reqwest::redirect::Policy::none());
        Self {
            db,
            client,
            allow_private,
            max_concurrent: 1,
        }
    }

    /// Sends up to `most` deliveries at once instead.
    #[must_use]
    pub fn max_concurrent(mut self, most: u32) -> Self {
        self.max_concurrent = most;
        self
    }

    pub fn register(self, registry: &mut Registry) {
        let pruner = self.db.clone();
        let most = self.max_concurrent;
        registry
            .register(move |job: DeliverWebhook| {
                let jobs = self.clone();
                async move { jobs.deliver(job.delivery_id).await }
            })
            // Each waits on a receiver, for up to the whole timeout.
            .at_most::<DeliverWebhook>(most);
        registry
            .register(move |_: PruneWebhookDeliveries| {
                let db = pruner.clone();
                async move {
                    webhooks::prune(&db, KEEP_DAYS).await?;
                    Ok(())
                }
            })
            .every::<PruneWebhookDeliveries>(Duration::from_secs(24 * 60 * 60));
    }

    /// Sends delivery `id`, in its webhook's format. Failures the receiver
    /// might recover from (network errors, 5xx, 408, 429) are retried with
    /// backoff, or when a 429 says; others are recorded and given up. After
    /// [`PAUSE_AFTER`] network errors in a row, deliveries to the webhook
    /// wait for [`PAUSE`] without sending.
    pub async fn deliver(&self, id: i64) -> Result<(), JobError> {
        let Some(delivery) = webhooks::delivery(&self.db, id).await? else {
            return Ok(());
        };
        if delivery.status == "delivered" {
            return Ok(());
        }
        let Some(hook) = webhooks::by_id(&self.db, delivery.webhook_id).await? else {
            return Ok(());
        };
        let give_up = |why: &str| {
            let why = why.to_owned();
            async move { webhooks::record_attempt(&self.db, id, false, None, &why).await }
        };
        if !hook.is_enabled && delivery.event != Event::Ping.as_str() {
            give_up("the webhook is turned off").await?;
            return Ok(());
        }
        let url = match url::Url::parse(&hook.url) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => url,
            _ => {
                give_up("the URL isn't a web address").await?;
                return Ok(());
            }
        };
        if !url
            .host()
            .is_some_and(|host| moekura_net::host_allowed(host, self.allow_private))
        {
            give_up("the address isn't on the public internet").await?;
            return Ok(());
        }
        // A receiver that keeps not answering is left alone for a while,
        // rather than every delivery holding a worker for the whole
        // timeout; they're retried as usual meanwhile. A test still goes,
        // to see whether it's back.
        let test = delivery.event == Event::Ping.as_str();
        if let Some(until) = hook
            .paused_until
            .filter(|until| !test && *until > OffsetDateTime::now_utc())
        {
            let why = format!(
                "not sent: {} tries in a row couldn't reach the receiver, so deliveries \
                 wait until {}",
                hook.network_failures,
                until.format(&Rfc3339).unwrap_or_default()
            );
            webhooks::record_attempt(&self.db, id, false, None, &why).await?;
            return Err(JobError::Retry(why));
        }

        let created_at = delivery.created_at.format(&Rfc3339).unwrap_or_default();
        let format = Format::parse(&hook.format).unwrap_or(Format::Moekura);
        let request = match format {
            Format::Moekura => {
                let body = serde_json::to_vec(&serde_json::json!({
                    "id": delivery.id,
                    "event": delivery.event,
                    "created_at": created_at,
                    "data": delivery.payload,
                }))
                .map_err(JobError::permanent)?;
                let timestamp = OffsetDateTime::now_utc().unix_timestamp();
                self.client
                    .post(url)
                    .header("x-moekura-event", &delivery.event)
                    .header("x-moekura-delivery", delivery.id.to_string())
                    .header("x-moekura-timestamp", timestamp.to_string())
                    .header(
                        "x-moekura-signature",
                        format!("sha256={}", signature(&hook.secret, timestamp, &body)),
                    )
                    .body(body)
            }
            // The URL's token is the secret; Discord has no use for ours.
            Format::Discord => {
                let options = discord::Options {
                    username: &hook.username,
                    avatar_url: &hook.avatar_url,
                    image_ratings: &hook.image_ratings,
                };
                let message =
                    discord::message(&delivery.event, &delivery.payload, &created_at, &options);
                let body = serde_json::to_vec(&message).map_err(JobError::permanent)?;
                self.client.post(discord::wait_url(&url)).body(body)
            }
        };
        let sent = request
            .header("content-type", "application/json")
            .send()
            .await;
        match sent {
            Ok(response) => {
                let status = response.status();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<f64>().ok());
                let (body, broke) = read_start(response, RESPONSE_READ).await;
                let mut text = String::from_utf8_lossy(&body).into_owned();
                // An answer that stalls held the worker as long as one
                // that never came.
                match broke {
                    Some(error) => {
                        if text.is_empty() {
                            text = format!("couldn't read the answer: {}", describe(error));
                        }
                        webhooks::note_unreachable(&self.db, hook.id, PAUSE_AFTER, PAUSE).await?;
                    }
                    None => webhooks::note_reachable(&self.db, hook.id).await?,
                }
                if format == Format::Discord && status.is_success() {
                    let message: Option<serde_json::Value> = serde_json::from_str(&text).ok();
                    if let Some(id) = message.as_ref().and_then(|m| m["id"].as_str()) {
                        text = format!("message {id}");
                    }
                }
                // Discord says how long to wait in the body, to the
                // millisecond; others in the header.
                let retry_after = (status.as_u16() == 429)
                    .then(|| {
                        serde_json::from_str::<serde_json::Value>(&text)
                            .ok()
                            .and_then(|body| body["retry_after"].as_f64())
                            .or(retry_after)
                    })
                    .flatten();
                if text.len() > RESPONSE_KEPT {
                    let mut end = RESPONSE_KEPT;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                }
                let code = Some(i32::from(status.as_u16()));
                webhooks::record_attempt(&self.db, id, status.is_success(), code, &text).await?;
                let retry = status.is_server_error() || matches!(status.as_u16(), 408 | 429);
                if status.is_success() || !retry {
                    Ok(())
                } else if let Some(wait) = retry_after.filter(|w| w.is_finite()) {
                    let wait = Duration::from_secs_f64(wait.clamp(1.0, MAX_RETRY_AFTER));
                    Err(JobError::RetryIn(
                        format!("the webhook answered {status}; waiting {wait:?}"),
                        wait,
                    ))
                } else {
                    Err(JobError::Retry(format!("the webhook answered {status}")))
                }
            }
            Err(error) => {
                let mut why = format!("couldn't send: {}", describe(error));
                if let Some(until) =
                    webhooks::note_unreachable(&self.db, hook.id, PAUSE_AFTER, PAUSE).await?
                {
                    let until = until.format(&Rfc3339).unwrap_or_default();
                    let _ = write!(why, "; paused until {until}");
                }
                webhooks::record_attempt(&self.db, id, false, None, &why).await?;
                Err(JobError::Retry(why))
            }
        }
    }
}

/// The first `max` bytes of `response`'s body, or as many as came before
/// it ended or failed (with why). The connection is dropped rather than
/// read to the end, so a receiver can't make a delivery download more.
async fn read_start(
    mut response: reqwest::Response,
    max: usize,
) -> (Vec<u8>, Option<reqwest::Error>) {
    let mut body = Vec::new();
    while body.len() < max {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = max - body.len();
                body.extend_from_slice(&chunk[..chunk.len().min(room)]);
            }
            Ok(None) => break,
            Err(error) => return (body, Some(error)),
        }
    }
    (body, None)
}

/// What went wrong with a request, with its causes but not its URL: a
/// Discord webhook's token is in the path, and the error is kept with the
/// delivery and the job.
fn describe(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut why = error.to_string();
    let mut cause = std::error::Error::source(&error);
    while let Some(error) = cause {
        let _ = write!(why, ": {error}");
        cause = error.source();
    }
    why
}

#[cfg(test)]
mod tests {
    use moekura_core::jobs::Job;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// Answers one request with `status`, returning what was sent.
    async fn receiver(status: u16) -> (String, tokio::task::JoinHandle<String>) {
        replying(status, "", "ok").await
    }

    /// Answers one request with `status`, extra `headers` (each ending in
    /// CRLF) and `body`, returning what was sent.
    async fn replying(
        status: u16,
        headers: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let received = take_request(&mut socket).await;
            let reply = format!(
                "HTTP/1.1 {status} X\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(reply.as_bytes()).await.unwrap();
            received
        });
        (url, task)
    }

    /// Reads a request from `socket`, returning it.
    async fn take_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = socket.read(&mut buf).await.unwrap();
            received.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&received).to_string();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        String::from_utf8_lossy(&received).to_string()
    }

    /// Answers one request with a 200 whose body says it's a gigabyte but
    /// stops after `sent` bytes, keeping the connection open.
    async fn endless(sent: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            take_request(&mut socket).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-length: 1000000000\r\n\r\n";
            socket.write_all(head.as_bytes()).await.unwrap();
            // The client may hang up before it all goes.
            let _ = socket.write_all(&vec![b'a'; sent]).await;
            tokio::time::sleep(Duration::from_secs(120)).await;
        });
        url
    }

    async fn delivery_to(pool: &PgPool, url: &str) -> i64 {
        let fields = webhooks::Fields::new(url, &[Event::PostCreated]);
        delivery_with(pool, &fields).await
    }

    async fn delivery_with(pool: &PgPool, fields: &webhooks::Fields) -> i64 {
        let hook = webhooks::create(pool, fields, "topsecret").await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        webhooks::emit(
            &mut conn,
            Event::PostCreated,
            &json!({ "post_id": 7, "rating": "g", "tags": ["cat"] }),
            Some(hook),
        )
        .await
        .unwrap()[0]
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn delivers_signed_requests(pool: PgPool) {
        let jobs = WebhookJobs::new(pool.clone(), Duration::from_secs(5), true);
        let (url, request) = receiver(200).await;
        let id = delivery_to(&pool, &url).await;
        jobs.deliver(id).await.unwrap();
        let request = request.await.unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        let header = |name: &str| {
            head.lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix(&format!("{name}: "))
                        .map(str::to_owned)
                })
                .unwrap()
        };
        assert_eq!(header("x-moekura-event"), "post.created");
        let timestamp: i64 = header("x-moekura-timestamp").parse().unwrap();
        assert_eq!(
            header("x-moekura-signature"),
            format!(
                "sha256={}",
                signature("topsecret", timestamp, body.as_bytes())
            )
        );
        let sent: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            (&sent["id"], &sent["data"]["post_id"]),
            (&json!(id), &json!(7))
        );
        let done = webhooks::delivery(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (done.status.as_str(), done.response_status),
            ("delivered", Some(200))
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn retries_what_might_recover(pool: PgPool) {
        let jobs = WebhookJobs::new(pool.clone(), Duration::from_secs(5), true);
        let (url, _) = receiver(503).await;
        let id = delivery_to(&pool, &url).await;
        assert!(matches!(jobs.deliver(id).await, Err(JobError::Retry(_))));

        let (url, _) = receiver(404).await;
        let id = delivery_to(&pool, &url).await;
        jobs.deliver(id).await.unwrap();
        let failed = webhooks::delivery(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (failed.status.as_str(), failed.response_status),
            ("failed", Some(404))
        );

        // Private addresses, unless allowed.
        let strict = WebhookJobs::new(pool.clone(), Duration::from_secs(5), false);
        let id = delivery_to(&pool, "http://127.0.0.1:9/hook").await;
        strict.deliver(id).await.unwrap();
        let refused = webhooks::delivery(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            refused.response.as_deref(),
            Some("the address isn't on the public internet")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn reads_only_the_start_of_answers(pool: PgPool) {
        let jobs = WebhookJobs::new(pool.clone(), Duration::from_secs(60), true);
        let url = endless(4 * RESPONSE_READ).await;
        let id = delivery_to(&pool, &url).await;
        // Done once it has enough, not when the rest would have come.
        tokio::time::timeout(Duration::from_secs(20), jobs.deliver(id))
            .await
            .expect("the rest isn't waited for")
            .unwrap();
        let done = webhooks::delivery(&pool, id).await.unwrap().unwrap();
        assert_eq!(done.status, "delivered");
        assert_eq!(done.response.unwrap(), "a".repeat(RESPONSE_KEPT));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn receivers_that_dont_answer_are_paused(pool: PgPool) {
        let jobs = WebhookJobs::new(pool.clone(), Duration::from_secs(5), true);
        let url = format!("http://{}/hook", closed_port().await);
        let fields = webhooks::Fields::new(&url, &[Event::PostCreated]);
        let hook = webhooks::create(&pool, &fields, "s").await.unwrap();
        let emit = |event: Event| {
            let pool = pool.clone();
            async move {
                let mut conn = pool.acquire().await.unwrap();
                webhooks::emit(&mut conn, event, &json!({ "post_id": 7 }), Some(hook))
                    .await
                    .unwrap()[0]
            }
        };
        for n in 1..=PAUSE_AFTER {
            let id = emit(Event::PostCreated).await;
            let Err(JobError::Retry(why)) = jobs.deliver(id).await else {
                panic!("a network error is retried");
            };
            assert_eq!(why.contains("; paused until "), n == PAUSE_AFTER, "{why}");
        }

        // Pointed elsewhere behind its back: paused deliveries don't try.
        let (live, request) = receiver(200).await;
        sqlx::query("UPDATE webhooks SET url = $2 WHERE id = $1")
            .bind(hook)
            .bind(&live)
            .execute(&pool)
            .await
            .unwrap();
        let waiting = emit(Event::PostCreated).await;
        let Err(JobError::Retry(why)) = jobs.deliver(waiting).await else {
            panic!("a paused delivery is retried later");
        };
        assert!(why.starts_with("not sent: 5 tries in a row"), "{why}");
        let recorded = webhooks::delivery(&pool, waiting).await.unwrap().unwrap();
        assert_eq!(recorded.response.as_deref(), Some(why.as_str()));
        assert!(!request.is_finished(), "nothing was sent");

        // A test still goes, and an answer ends the pause.
        let test = emit(Event::Ping).await;
        jobs.deliver(test).await.unwrap();
        assert!(request.await.unwrap().contains("x-moekura-event: ping"));
        let hook = webhooks::by_id(&pool, hook).await.unwrap().unwrap();
        assert_eq!((hook.network_failures, hook.paused_until), (0, None));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn stalled_answers_count_as_failures(pool: PgPool) {
        let jobs = WebhookJobs::new(pool.clone(), Duration::from_secs(1), true);
        let url = endless(10).await;
        let id = delivery_to(&pool, &url).await;
        jobs.deliver(id).await.unwrap();
        let done = webhooks::delivery(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (done.status.as_str(), done.response.as_deref()),
            ("delivered", Some("aaaaaaaaaa"))
        );
        let hook = webhooks::by_id(&pool, done.webhook_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hook.network_failures, 1);
    }

    #[tokio::test]
    async fn deliveries_are_limited() {
        let pool = PgPool::connect_lazy("postgres://localhost/unused").unwrap();
        let mut registry = Registry::new();
        WebhookJobs::new(pool, Duration::from_secs(5), true)
            .max_concurrent(3)
            .register(&mut registry);
        assert_eq!(registry.limits, [(DeliverWebhook::KIND, 3)]);
    }

    /// A local address nothing listens on.
    async fn closed_port() -> std::net::SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn network_errors_leave_the_url_out(pool: PgPool) {
        let jobs = WebhookJobs::new(pool.clone(), Duration::from_secs(5), true);
        // Like a Discord webhook, whose token is in the path.
        let url = format!("http://{}/api/webhooks/1/tok3n", closed_port().await);
        let id = delivery_with(&pool, &discord_hook(&url)).await;
        let Err(JobError::Retry(why)) = jobs.deliver(id).await else {
            panic!("a network error is retried");
        };
        let failed = webhooks::delivery(&pool, id).await.unwrap().unwrap();
        let stored = failed.response.unwrap();
        assert_eq!(stored, why);
        assert!(stored.starts_with("couldn't send: "), "{stored}");
        assert!(
            !stored.contains("tok3n") && !stored.contains("127.0.0.1"),
            "{stored}"
        );
        // But with what went wrong.
        assert!(stored.to_lowercase().contains("connect"), "{stored}");
    }

    /// A webhook in Discord's format, to `url`.
    fn discord_hook(url: &str) -> webhooks::Fields {
        webhooks::Fields {
            format: Format::Discord,
            username: "Booru".into(),
            ..webhooks::Fields::new(url, &[Event::PostCreated])
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn delivers_discord_messages(pool: PgPool) {
        let jobs = WebhookJobs::new(pool.clone(), Duration::from_secs(5), true);
        let (url, request) = replying(200, "", r#"{"id":"5550001","type":0}"#).await;
        let id = delivery_with(&pool, &discord_hook(&url)).await;
        jobs.deliver(id).await.unwrap();
        let request = request.await.unwrap();
        let (head, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /hook?wait=true "), "{head}");
        assert!(!head.to_lowercase().contains("x-moekura"), "{head}");
        let sent: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(sent["embeds"][0]["title"], json!("Post #7 uploaded"));
        assert_eq!(sent["username"], json!("Booru"));
        assert_eq!(sent["allowed_mentions"], json!({ "parse": [] }));
        let done = webhooks::delivery(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (done.status.as_str(), done.response.as_deref()),
            ("delivered", Some("message 5550001"))
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn waits_as_long_as_rate_limits_say(pool: PgPool) {
        let jobs = WebhookJobs::new(pool.clone(), Duration::from_secs(5), true);
        let (url, _) = replying(
            429,
            "retry-after: 3\r\n",
            r#"{"message":"You are being rate limited.","retry_after":2.5,"global":false}"#,
        )
        .await;
        let id = delivery_with(&pool, &discord_hook(&url)).await;
        match jobs.deliver(id).await {
            Err(JobError::RetryIn(_, wait)) => assert_eq!(wait, Duration::from_millis(2500)),
            other => panic!("{other:?}"),
        }

        let (url, _) = replying(429, "retry-after: 7\r\n", "slow down").await;
        let id = delivery_to(&pool, &url).await;
        match jobs.deliver(id).await {
            Err(JobError::RetryIn(_, wait)) => assert_eq!(wait, Duration::from_secs(7)),
            other => panic!("{other:?}"),
        }

        // Without a wait, the usual backoff; other 4xx give up.
        let (url, _) = receiver(429).await;
        let id = delivery_with(&pool, &discord_hook(&url)).await;
        assert!(matches!(jobs.deliver(id).await, Err(JobError::Retry(_))));
        let (url, _) = replying(400, "", r#"{"message":"Cannot send an empty message"}"#).await;
        let id = delivery_with(&pool, &discord_hook(&url)).await;
        jobs.deliver(id).await.unwrap();
        let failed = webhooks::delivery(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (failed.status.as_str(), failed.response_status),
            ("failed", Some(400))
        );
    }
}
