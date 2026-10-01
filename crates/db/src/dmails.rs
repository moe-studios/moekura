//! Private messages (`dmails`) and blocks between users (`user_blocks`).

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Dmail {
    pub id: i64,
    pub owner_id: i64,
    pub from_id: Option<i64>,
    pub from_name: Option<String>,
    pub to_id: Option<i64>,
    pub to_name: Option<String>,
    pub title: String,
    pub body: String,
    pub is_read: bool,
    pub is_deleted: bool,
    pub reported_at: Option<OffsetDateTime>,
    pub report_reason: String,
    pub created_at: OffsetDateTime,
}

/// `SELECT <dmail columns> FROM dmails d …` followed by `$rest`.
macro_rules! select_dmails {
    ($rest:literal) => {
        concat!(
            "SELECT d.id, d.owner_id, d.from_id, f.name::text AS from_name, d.to_id,
                    t.name::text AS to_name, d.title, d.body, d.is_read, d.is_deleted,
                    d.reported_at, d.report_reason, d.created_at
             FROM dmails d
             LEFT JOIN users f ON f.id = d.from_id
             LEFT JOIN users t ON t.id = d.to_id ",
            $rest
        )
    };
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// The recipient blocked the sender.
    #[error("the recipient doesn't take messages from you")]
    Blocked,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// A sent message's two copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sent {
    pub sender_copy: i64,
    pub recipient_copy: i64,
}

/// Sends a message: the sender's copy and the recipient's.
pub async fn send(
    db: &PgPool,
    from_id: i64,
    to_id: i64,
    title: &str,
    body: &str,
) -> Result<Sent, SendError> {
    let mut tx = db.begin().await?;
    if is_blocked(&mut *tx, to_id, from_id).await? {
        return Err(SendError::Blocked);
    }
    let insert = "INSERT INTO dmails (owner_id, from_id, to_id, title, body, is_read)
                  VALUES ($1, $2, $3, $4, $5, $6) RETURNING id";
    let sent: i64 = sqlx::query_scalar(insert)
        .bind(from_id)
        .bind(from_id)
        .bind(to_id)
        .bind(title)
        .bind(body)
        .bind(true)
        .fetch_one(&mut *tx)
        .await?;
    // Messages to oneself are just the one copy.
    let received = if from_id == to_id {
        sent
    } else {
        sqlx::query_scalar(insert)
            .bind(to_id)
            .bind(from_id)
            .bind(to_id)
            .bind(title)
            .bind(body)
            .bind(false)
            .fetch_one(&mut *tx)
            .await?
    };
    tx.commit().await?;
    Ok(Sent {
        sender_copy: sent,
        recipient_copy: received,
    })
}

/// Which messages [`list`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    /// Received.
    Inbox,
    Sent,
    All,
}

/// `owner_id`'s messages in `folder`, newest first; with `unread`, only
/// those not read yet.
pub async fn list(
    db: impl PgExecutor<'_>,
    owner_id: i64,
    folder: Folder,
    unread: bool,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Dmail>> {
    let folder = match folder {
        Folder::Inbox => "inbox",
        Folder::Sent => "sent",
        Folder::All => "all",
    };
    sqlx::query_as(select_dmails!(
        "WHERE d.owner_id = $1 AND NOT d.is_deleted
           AND ($2 = 'all'
                OR ($2 = 'inbox' AND d.to_id IS NOT DISTINCT FROM d.owner_id
                    AND d.from_id IS DISTINCT FROM d.owner_id)
                OR ($2 = 'sent' AND d.from_id = d.owner_id))
           AND (NOT $3 OR NOT d.is_read)
         ORDER BY d.id DESC OFFSET $4 LIMIT $5"
    ))
    .bind(owner_id)
    .bind(folder)
    .bind(unread)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Message `id`, if it's `owner_id`'s copy (deleted ones included).
pub async fn by_id(db: impl PgExecutor<'_>, owner_id: i64, id: i64) -> sqlx::Result<Option<Dmail>> {
    sqlx::query_as(select_dmails!("WHERE d.id = $1 AND d.owner_id = $2"))
        .bind(id)
        .bind(owner_id)
        .fetch_optional(db)
        .await
}

/// Any copy `id`, for staff looking at a report.
pub async fn by_id_any(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Dmail>> {
    sqlx::query_as(select_dmails!("WHERE d.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn unread_count(db: impl PgExecutor<'_>, owner_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM dmails WHERE owner_id = $1 AND NOT is_read AND NOT is_deleted",
    )
    .bind(owner_id)
    .fetch_one(db)
    .await
}

/// Marks `owner_id`'s message `id` read or unread.
pub async fn set_read(
    db: impl PgExecutor<'_>,
    owner_id: i64,
    id: i64,
    read: bool,
) -> sqlx::Result<bool> {
    let done = sqlx::query("UPDATE dmails SET is_read = $3 WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(owner_id)
        .bind(read)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn mark_all_read(db: impl PgExecutor<'_>, owner_id: i64) -> sqlx::Result<u64> {
    Ok(sqlx::query(
        "UPDATE dmails SET is_read = true WHERE owner_id = $1 AND NOT is_read AND NOT is_deleted",
    )
    .bind(owner_id)
    .execute(db)
    .await?
    .rows_affected())
}

/// Deletes (or restores) `owner_id`'s copy of message `id`.
pub async fn set_deleted(
    db: impl PgExecutor<'_>,
    owner_id: i64,
    id: i64,
    deleted: bool,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE dmails SET is_deleted = $3, is_read = is_read OR $3 WHERE id = $1 AND owner_id = $2",
    )
    .bind(id)
    .bind(owner_id)
    .bind(deleted)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Reports `owner_id`'s received message `id` to staff. False if it
/// isn't theirs or they sent it.
pub async fn report(
    db: impl PgExecutor<'_>,
    owner_id: i64,
    id: i64,
    reason: &str,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE dmails SET reported_at = now(), report_reason = $3, report_settled = false
         WHERE id = $1 AND owner_id = $2 AND from_id IS DISTINCT FROM owner_id",
    )
    .bind(id)
    .bind(owner_id)
    .bind(reason)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Reported messages not settled yet, the latest first.
pub async fn reported(
    db: impl PgExecutor<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Dmail>> {
    sqlx::query_as(select_dmails!(
        "WHERE d.reported_at IS NOT NULL AND NOT d.report_settled
         ORDER BY d.reported_at DESC OFFSET $1 LIMIT $2"
    ))
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Settles the report about message `id`.
pub async fn settle_report(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE dmails SET report_settled = true
         WHERE id = $1 AND reported_at IS NOT NULL AND NOT report_settled",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Whether `user_id` blocked `other_id`.
pub async fn is_blocked(
    db: impl PgExecutor<'_>,
    user_id: i64,
    other_id: i64,
) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_blocks WHERE user_id = $1 AND blocked_id = $2)",
    )
    .bind(user_id)
    .bind(other_id)
    .fetch_one(db)
    .await
}

/// Blocks (or unblocks) `other_id`'s messages for `user_id`.
pub async fn set_blocked(
    db: impl PgExecutor<'_>,
    user_id: i64,
    other_id: i64,
    blocked: bool,
) -> sqlx::Result<()> {
    let sql = if blocked {
        "INSERT INTO user_blocks (user_id, blocked_id) VALUES ($1, $2) ON CONFLICT DO NOTHING"
    } else {
        "DELETE FROM user_blocks WHERE user_id = $1 AND blocked_id = $2"
    };
    sqlx::query(sql)
        .bind(user_id)
        .bind(other_id)
        .execute(db)
        .await?;
    Ok(())
}

/// The users `user_id` blocked, by name.
pub async fn blocked(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT u.name::text FROM user_blocks b JOIN users u ON u.id = b.blocked_id
         WHERE b.user_id = $1 ORDER BY lower(u.name)",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id)
             VALUES ($1, (SELECT id FROM roles WHERE system_key = 'member')) RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn sending_reading_and_blocking(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let id = send(&pool, alice, bob, "Hi", "Hello there")
            .await
            .unwrap()
            .recipient_copy;
        assert_eq!(unread_count(&pool, bob).await.unwrap(), 1);
        assert_eq!(unread_count(&pool, alice).await.unwrap(), 0);
        let inbox = list(&pool, bob, Folder::Inbox, false, 0, 10).await.unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].from_name.as_deref(), Some("alice"));
        assert!(
            list(&pool, bob, Folder::Sent, false, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            list(&pool, alice, Folder::Sent, false, 0, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        // Another's copy can't be read through its id.
        assert!(by_id(&pool, alice, id).await.unwrap().is_none());

        assert!(set_read(&pool, bob, id, true).await.unwrap());
        assert_eq!(unread_count(&pool, bob).await.unwrap(), 0);
        assert!(report(&pool, bob, id, "spam").await.unwrap());
        assert_eq!(reported(&pool, 0, 10).await.unwrap().len(), 1);
        assert!(settle_report(&pool, id).await.unwrap());
        assert!(reported(&pool, 0, 10).await.unwrap().is_empty());

        set_blocked(&pool, bob, alice, true).await.unwrap();
        assert!(matches!(
            send(&pool, alice, bob, "Hi", "Again").await,
            Err(SendError::Blocked)
        ));
        assert_eq!(blocked(&pool, bob).await.unwrap(), ["alice"]);
        set_blocked(&pool, bob, alice, false).await.unwrap();
        send(&pool, alice, bob, "Hi", "Again").await.unwrap();
        assert_eq!(mark_all_read(&pool, bob).await.unwrap(), 1);
        assert!(set_deleted(&pool, bob, id, true).await.unwrap());
        assert_eq!(
            list(&pool, bob, Folder::All, false, 0, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
