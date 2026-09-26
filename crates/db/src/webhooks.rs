//! Outgoing webhooks and their deliveries.

use moekura_core::jobs::DeliverWebhook;
use moekura_core::webhooks::Event;
use serde_json::Value;
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Webhook {
    pub id: i32,
    pub url: String,
    pub description: String,
    pub secret: String,
    pub events: Vec<String>,
    pub is_enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

pub async fn list(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<Webhook>> {
    sqlx::query_as("SELECT * FROM webhooks ORDER BY id")
        .fetch_all(db)
        .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<Option<Webhook>> {
    sqlx::query_as("SELECT * FROM webhooks WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn create(
    db: impl PgExecutor<'_>,
    url: &str,
    description: &str,
    secret: &str,
    events: &[String],
) -> sqlx::Result<i32> {
    sqlx::query_scalar(
        "INSERT INTO webhooks (url, description, secret, events) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(url)
    .bind(description)
    .bind(secret)
    .bind(events)
    .fetch_one(db)
    .await
}

pub async fn update(
    db: impl PgExecutor<'_>,
    id: i32,
    url: &str,
    description: &str,
    events: &[String],
    is_enabled: bool,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE webhooks SET url = $2, description = $3, events = $4, is_enabled = $5,
                             updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(url)
    .bind(description)
    .bind(events)
    .bind(is_enabled)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn set_secret(db: impl PgExecutor<'_>, id: i32, secret: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE webhooks SET secret = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(secret)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn delete(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM webhooks WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Queues a delivery of `event` with `data` to every enabled webhook
/// subscribed to it (or, for `only`, to that webhook alone), in the
/// caller's transaction. Returns the deliveries' ids.
pub async fn emit(
    conn: &mut PgConnection,
    event: Event,
    data: &Value,
    only: Option<i32>,
) -> sqlx::Result<Vec<i64>> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "INSERT INTO webhook_deliveries (webhook_id, event, payload)
         SELECT id, $1, $2 FROM webhooks
         WHERE CASE WHEN $3::int IS NULL THEN is_enabled AND $1 = ANY(events) ELSE id = $3 END
         RETURNING id",
    )
    .bind(event.as_str())
    .bind(data)
    .bind(only)
    .fetch_all(&mut *conn)
    .await?;
    for &delivery_id in &ids {
        crate::jobs::enqueue(conn, &DeliverWebhook { delivery_id }).await?;
    }
    Ok(ids)
}

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Delivery {
    pub id: i64,
    pub webhook_id: i32,
    pub event: String,
    pub payload: Value,
    pub status: String,
    pub attempts: i32,
    pub response_status: Option<i32>,
    pub response: Option<String>,
    pub created_at: OffsetDateTime,
    pub delivered_at: Option<OffsetDateTime>,
}

pub async fn delivery(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Delivery>> {
    sqlx::query_as("SELECT * FROM webhook_deliveries WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// A webhook's latest deliveries, newest first.
pub async fn deliveries(
    db: impl PgExecutor<'_>,
    webhook_id: i32,
    limit: i64,
) -> sqlx::Result<Vec<Delivery>> {
    sqlx::query_as(
        "SELECT * FROM webhook_deliveries WHERE webhook_id = $1 ORDER BY id DESC LIMIT $2",
    )
    .bind(webhook_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Records an attempt: delivered, or failed with what came back.
pub async fn record_attempt(
    db: impl PgExecutor<'_>,
    id: i64,
    delivered: bool,
    response_status: Option<i32>,
    response: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE webhook_deliveries
         SET attempts = attempts + 1,
             status = CASE WHEN $2 THEN 'delivered' ELSE 'failed' END,
             response_status = $3, response = $4,
             delivered_at = CASE WHEN $2 THEN now() END
         WHERE id = $1",
    )
    .bind(id)
    .bind(delivered)
    .bind(response_status)
    .bind(response)
    .execute(db)
    .await?;
    Ok(())
}

/// Removes deliveries older than `days`; returns how many.
pub async fn prune(db: impl PgExecutor<'_>, days: i32) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM webhook_deliveries WHERE created_at < now() - make_interval(days => $1)",
    )
    .bind(days)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn emits_to_subscribers(pool: PgPool) {
        let events = |e: &[Event]| e.iter().map(|e| e.as_str().to_owned()).collect::<Vec<_>>();
        let posts = create(
            &pool,
            "https://a.example/hook",
            "",
            "s1",
            &events(&[Event::PostCreated]),
        )
        .await
        .unwrap();
        let comments = create(
            &pool,
            "https://b.example/hook",
            "",
            "s2",
            &events(&[Event::CommentCreated]),
        )
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let sent = emit(
            &mut conn,
            Event::PostCreated,
            &json!({ "post_id": 1 }),
            None,
        )
        .await
        .unwrap();
        assert_eq!(sent.len(), 1);
        let first = delivery(&pool, sent[0]).await.unwrap().unwrap();
        assert_eq!((first.webhook_id, first.status.as_str()), (posts, "queued"));
        let jobs: i64 =
            sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'webhooks.deliver'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(jobs, 1);

        // Disabled webhooks get nothing, except a test aimed at them.
        let hook = by_id(&pool, comments).await.unwrap().unwrap();
        update(&pool, comments, &hook.url, "", &hook.events, false)
            .await
            .unwrap();
        assert!(
            emit(&mut conn, Event::CommentCreated, &json!({}), None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            emit(&mut conn, Event::Ping, &json!({}), Some(comments))
                .await
                .unwrap()
                .len(),
            1
        );

        record_attempt(&pool, sent[0], false, Some(500), "oops")
            .await
            .unwrap();
        record_attempt(&pool, sent[0], true, Some(200), "ok")
            .await
            .unwrap();
        let done = delivery(&pool, sent[0]).await.unwrap().unwrap();
        assert_eq!((done.status.as_str(), done.attempts), ("delivered", 2));
        assert_eq!(deliveries(&pool, posts, 10).await.unwrap().len(), 1);
        assert_eq!(prune(&pool, 30).await.unwrap(), 0);
    }
}
