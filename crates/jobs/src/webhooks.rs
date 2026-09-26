//! The `webhooks.deliver` job: sending one delivery, signed, to its
//! webhook; and `webhooks.prune`, forgetting old deliveries.

use std::time::Duration;

use moekura_core::jobs::{DeliverWebhook, PruneWebhookDeliveries};
use moekura_core::webhooks::{Event, signature};
use moekura_db::webhooks;
use sqlx::PgPool;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{JobError, Registry};

/// How long deliveries are kept, in days.
const KEEP_DAYS: i32 = 30;

/// How much of a response is kept.
const RESPONSE_KEPT: usize = 1000;

#[derive(Clone)]
pub struct WebhookJobs {
    pub db: PgPool,
    pub client: reqwest::Client,
    pub allow_private: bool,
}

impl WebhookJobs {
    /// A sender whose connections only go to public addresses (unless
    /// `allow_private`), following no redirects.
    pub fn new(db: PgPool, timeout: Duration, allow_private: bool) -> Self {
        let client = moekura_net::client(timeout, allow_private, reqwest::redirect::Policy::none());
        Self {
            db,
            client,
            allow_private,
        }
    }

    pub fn register(self, registry: &mut Registry) {
        let pruner = self.db.clone();
        registry.register(move |job: DeliverWebhook| {
            let jobs = self.clone();
            async move { jobs.deliver(job.delivery_id).await }
        });
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

    /// Sends delivery `id`. Failures the receiver might recover from
    /// (network errors, 5xx, 408, 429) are retried with backoff; others
    /// are recorded and given up.
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

        let body = serde_json::to_vec(&serde_json::json!({
            "id": delivery.id,
            "event": delivery.event,
            "created_at": delivery.created_at.format(&Rfc3339).unwrap_or_default(),
            "data": delivery.payload,
        }))
        .map_err(JobError::permanent)?;
        let timestamp = OffsetDateTime::now_utc().unix_timestamp();
        let sent = self
            .client
            .post(url)
            .header("content-type", "application/json")
            .header("x-moekura-event", &delivery.event)
            .header("x-moekura-delivery", delivery.id.to_string())
            .header("x-moekura-timestamp", timestamp.to_string())
            .header(
                "x-moekura-signature",
                format!("sha256={}", signature(&hook.secret, timestamp, &body)),
            )
            .body(body)
            .send()
            .await;
        match sent {
            Ok(response) => {
                let status = response.status();
                let mut text = response.text().await.unwrap_or_default();
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
                } else {
                    Err(JobError::Retry(format!("the webhook answered {status}")))
                }
            }
            Err(error) => {
                let why = format!("couldn't send: {error}");
                webhooks::record_attempt(&self.db, id, false, None, &why).await?;
                Err(JobError::Retry(why))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// Answers one request with `status`, returning what was sent.
    async fn receiver(status: u16) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
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
            let reply =
                format!("HTTP/1.1 {status} X\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
            socket.write_all(reply.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&received).to_string()
        });
        (url, task)
    }

    async fn delivery_to(pool: &PgPool, url: &str) -> i64 {
        let hook = webhooks::create(pool, url, "", "topsecret", &["post.created".to_owned()])
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        webhooks::emit(
            &mut conn,
            Event::PostCreated,
            &json!({ "post_id": 7 }),
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
}
