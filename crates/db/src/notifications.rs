//! Notifications (`notifications`): mentions, replies, messages, forum
//! posts and request decisions, for the people concerned.

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

/// What a notification is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mention,
    Reply,
    Message,
    Forum,
    Request,
    /// Feedback left on one's account.
    Feedback,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Mention => "mention",
            Kind::Reply => "reply",
            Kind::Message => "message",
            Kind::Forum => "forum",
            Kind::Request => "request",
            Kind::Feedback => "feedback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Notification {
    pub id: i64,
    pub user_id: i64,
    pub kind: String,
    pub actor_name: Option<String>,
    pub subject: String,
    pub url: String,
    pub is_read: bool,
    pub created_at: OffsetDateTime,
}

/// Notifies each of `user_ids` (once each) of the same thing.
pub async fn create(
    db: impl PgExecutor<'_>,
    user_ids: &[i64],
    kind: Kind,
    actor_id: Option<i64>,
    subject: &str,
    url: &str,
) -> sqlx::Result<u64> {
    if user_ids.is_empty() {
        return Ok(0);
    }
    let subject: String = subject.chars().take(300).collect();
    Ok(sqlx::query(
        "INSERT INTO notifications (user_id, kind, actor_id, subject, url)
         SELECT DISTINCT u, $2, $3, $4, $5 FROM unnest($1::bigint[]) AS u",
    )
    .bind(user_ids)
    .bind(kind.as_str())
    .bind(actor_id)
    .bind(subject)
    .bind(url)
    .execute(db)
    .await?
    .rows_affected())
}

/// `user_id`'s notifications, newest first.
pub async fn list(
    db: impl PgExecutor<'_>,
    user_id: i64,
    unread: bool,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Notification>> {
    sqlx::query_as(
        "SELECT n.id, n.user_id, n.kind, u.name::text AS actor_name, n.subject, n.url, n.is_read,
                n.created_at
         FROM notifications n LEFT JOIN users u ON u.id = n.actor_id
         WHERE n.user_id = $1 AND (NOT $2 OR NOT n.is_read)
         ORDER BY n.id DESC OFFSET $3 LIMIT $4",
    )
    .bind(user_id)
    .bind(unread)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn unread_count(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM notifications WHERE user_id = $1 AND NOT is_read")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// Marks `user_id`'s notification `id` read, returning where it leads.
pub async fn read(db: impl PgExecutor<'_>, user_id: i64, id: i64) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "UPDATE notifications SET is_read = true WHERE id = $1 AND user_id = $2 RETURNING url",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// Marks `user_id`'s notifications leading to `url` read, as when they
/// open the thing itself.
pub async fn read_url(db: impl PgExecutor<'_>, user_id: i64, url: &str) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "UPDATE notifications SET is_read = true WHERE user_id = $1 AND url = $2 AND NOT is_read",
    )
    .bind(user_id)
    .bind(url)
    .execute(db)
    .await?
    .rows_affected())
}

pub async fn read_all(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<u64> {
    Ok(
        sqlx::query("UPDATE notifications SET is_read = true WHERE user_id = $1 AND NOT is_read")
            .bind(user_id)
            .execute(db)
            .await?
            .rows_affected(),
    )
}

/// Forgets read notifications older than `days`.
pub async fn prune(db: &PgPool, days: i32) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "DELETE FROM notifications WHERE is_read AND created_at < now() - make_interval(days => $1)",
    )
    .bind(days)
    .execute(db)
    .await?
    .rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn notifies_and_reads(pool: PgPool) {
        let alice: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id)
             VALUES ('alice', (SELECT id FROM roles WHERE system_key = 'member')) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        create(
            &pool,
            &[alice, alice],
            Kind::Mention,
            None,
            "post #1",
            "/posts/1",
        )
        .await
        .unwrap();
        assert_eq!(unread_count(&pool, alice).await.unwrap(), 1);
        let found = list(&pool, alice, true, 0, 10).await.unwrap();
        assert_eq!(found[0].kind, "mention");
        assert_eq!(
            read(&pool, alice, found[0].id).await.unwrap().as_deref(),
            Some("/posts/1")
        );
        assert_eq!(unread_count(&pool, alice).await.unwrap(), 0);
        create(&pool, &[alice], Kind::Message, None, "Hi", "/dmails/1")
            .await
            .unwrap();
        assert_eq!(read_all(&pool, alice).await.unwrap(), 1);
    }
}
