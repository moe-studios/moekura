//! Private messages (`dmails`) and blocks between users (`user_blocks`).

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

pub use crate::writing::Status;

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
    /// Not active: deleted, held or rejected (see `status`).
    pub is_deleted: bool,
    pub status: Status,
    pub created_at: OffsetDateTime,
}

/// `SELECT <dmail columns> FROM dmails d …` followed by `$rest`.
macro_rules! select_dmails {
    ($rest:literal) => {
        concat!(
            "SELECT d.id, d.owner_id, d.from_id, f.name::text AS from_name, d.to_id,
                    t.name::text AS to_name, d.title, d.body, d.is_read, d.is_deleted, d.status,
                    d.created_at
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
    held: Option<&str>,
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
        // A held copy stays out of the inbox until the staff approve it.
        sqlx::query_scalar(
            "INSERT INTO dmails (owner_id, from_id, to_id, title, body, is_read, status, held_reason)
             VALUES ($1, $2, $3, $4, $5, false, $6, $7) RETURNING id",
        )
        .bind(to_id)
        .bind(from_id)
        .bind(to_id)
        .bind(title)
        .bind(body)
        .bind(if held.is_some() {
            Status::Held
        } else {
            Status::Active
        })
        .bind(held)
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

/// Message `id`, if it's `owner_id`'s copy (deleted ones included, but
/// not those held for review or rejected).
pub async fn by_id(db: impl PgExecutor<'_>, owner_id: i64, id: i64) -> sqlx::Result<Option<Dmail>> {
    sqlx::query_as(select_dmails!(
        "WHERE d.id = $1 AND d.owner_id = $2 AND d.status IN ('active', 'deleted')"
    ))
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
    let done = sqlx::query(
        "UPDATE dmails SET is_read = $3
         WHERE id = $1 AND owner_id = $2 AND status IN ('active', 'deleted')",
    )
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

/// Deletes (or restores) `owner_id`'s copy of message `id`. Copies held
/// for review or rejected aren't the owner's to restore: false.
pub async fn set_deleted(
    db: impl PgExecutor<'_>,
    owner_id: i64,
    id: i64,
    deleted: bool,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE dmails SET status = CASE WHEN $3 THEN 'deleted' ELSE 'active' END,
                           is_read = is_read OR $3
         WHERE id = $1 AND owner_id = $2 AND status IN ('active', 'deleted')",
    )
    .bind(id)
    .bind(owner_id)
    .bind(deleted)
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

/// Those among `user_ids` who blocked `user_id`.
pub async fn blocking(
    db: impl PgExecutor<'_>,
    user_id: i64,
    user_ids: &[i64],
) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT user_id FROM user_blocks WHERE blocked_id = $1 AND user_id = ANY($2)",
    )
    .bind(user_id)
    .bind(user_ids)
    .fetch_all(db)
    .await
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
        let id = send(&pool, alice, bob, "Hi", "Hello there", None)
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

        set_blocked(&pool, bob, alice, true).await.unwrap();
        assert!(matches!(
            send(&pool, alice, bob, "Hi", "Again", None).await,
            Err(SendError::Blocked)
        ));
        assert_eq!(blocked(&pool, bob).await.unwrap(), ["alice"]);
        set_blocked(&pool, bob, alice, false).await.unwrap();
        send(&pool, alice, bob, "Hi", "Again", None).await.unwrap();
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
