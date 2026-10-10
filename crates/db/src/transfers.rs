//! Files sent in pieces (`file_transfers`), so that no one request has to
//! carry a whole file. The pieces themselves are in storage; this records
//! whose file it is, how large it will be, and how much of it has come.

use std::time::Duration;

use moekura_core::tokens::{NewToken, hash_token};
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

/// Most transfers one user may have unfinished (not yet used by a form).
pub const MAX_PER_USER: i64 = 40;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Transfer {
    pub id: i64,
    pub token_hash: Vec<u8>,
    pub uploader_id: i64,
    pub file_name: String,
    /// The file's size, as declared when it began.
    pub length: i64,
    /// Bytes received so far.
    pub received: i64,
    /// Pieces stored, numbered from 0.
    pub parts: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl Transfer {
    /// Whether the whole file has come.
    pub fn is_complete(&self) -> bool {
        self.received == self.length
    }
}

/// The columns of a [`Transfer`], for queries.
macro_rules! columns {
    () => {
        "id, token_hash, uploader_id, file_name, length, received, parts, created_at, updated_at"
    };
}

/// Begins a transfer of a file of `length` bytes named `file_name` for
/// `uploader_id`. Returns the token that names it, and the transfer.
pub async fn create(
    db: impl PgExecutor<'_>,
    uploader_id: i64,
    file_name: &str,
    length: i64,
) -> sqlx::Result<(String, Transfer)> {
    let token = NewToken::generate();
    let transfer = sqlx::query_as(concat!(
        "INSERT INTO file_transfers (token_hash, uploader_id, file_name, length)
         VALUES ($1, $2, $3, $4) RETURNING ",
        columns!()
    ))
    .bind(&token.hash[..])
    .bind(uploader_id)
    .bind(file_name)
    .bind(length)
    .fetch_one(db)
    .await?;
    Ok((token.token, transfer))
}

/// `uploader_id`'s transfer named `token`.
pub async fn find(
    db: impl PgExecutor<'_>,
    uploader_id: i64,
    token: &str,
) -> sqlx::Result<Option<Transfer>> {
    sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM file_transfers WHERE token_hash = $1 AND uploader_id = $2"
    ))
    .bind(&hash_token(token)[..])
    .bind(uploader_id)
    .fetch_optional(db)
    .await
}

/// Transfer `id`, locked until the transaction ends, so that pieces are
/// added one at a time.
pub async fn lock(conn: &mut PgConnection, id: i64) -> sqlx::Result<Option<Transfer>> {
    sqlx::query_as(concat!(
        "SELECT ",
        columns!(),
        " FROM file_transfers WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(conn)
    .await
}

/// Records the next piece of transfer `id`, of `bytes` bytes.
pub async fn add_part(conn: &mut PgConnection, id: i64, bytes: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE file_transfers
         SET received = received + $2, parts = parts + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(bytes)
    .execute(conn)
    .await?;
    Ok(())
}

/// Removes `uploader_id`'s transfer named `token`, returning it: to use
/// its file, or because the sender gave up on it. Only one caller gets it.
pub async fn take(
    db: impl PgExecutor<'_>,
    uploader_id: i64,
    token: &str,
) -> sqlx::Result<Option<Transfer>> {
    sqlx::query_as(concat!(
        "DELETE FROM file_transfers WHERE token_hash = $1 AND uploader_id = $2 RETURNING ",
        columns!()
    ))
    .bind(&hash_token(token)[..])
    .bind(uploader_id)
    .fetch_optional(db)
    .await
}

/// How many transfers `uploader_id` has unfinished.
pub async fn unfinished(db: impl PgExecutor<'_>, uploader_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM file_transfers WHERE uploader_id = $1")
        .bind(uploader_id)
        .fetch_one(db)
        .await
}

/// Removes up to `limit` transfers no piece came for in `idle`, returning
/// them, so their pieces can be removed from storage.
pub async fn take_idle(
    db: impl PgExecutor<'_>,
    idle: Duration,
    limit: i64,
) -> sqlx::Result<Vec<Transfer>> {
    sqlx::query_as(concat!(
        "DELETE FROM file_transfers WHERE id IN (
             SELECT id FROM file_transfers
             WHERE updated_at < now() - make_interval(secs => $1)
             ORDER BY updated_at LIMIT $2
             FOR UPDATE SKIP LOCKED
         ) RETURNING ",
        columns!()
    ))
    .bind(idle.as_secs_f64())
    .bind(limit)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn pieces_are_counted_and_the_file_taken_once(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let (token, transfer) = create(&pool, alice, "big.mp4", 10).await.unwrap();
        assert_eq!((transfer.received, transfer.parts), (0, 0));
        assert!(!transfer.is_complete());
        assert_eq!(unfinished(&pool, alice).await.unwrap(), 1);
        // Only its sender finds it.
        assert_eq!(find(&pool, bob, &token).await.unwrap(), None);
        assert_eq!(find(&pool, alice, "other").await.unwrap(), None);

        for bytes in [6, 4] {
            let mut tx = pool.begin().await.unwrap();
            let locked = lock(&mut tx, transfer.id).await.unwrap().unwrap();
            add_part(&mut tx, locked.id, bytes).await.unwrap();
            tx.commit().await.unwrap();
        }
        let found = find(&pool, alice, &token).await.unwrap().unwrap();
        assert_eq!((found.received, found.parts), (10, 2));
        assert!(found.is_complete());

        // More than the file's length isn't recorded.
        let mut tx = pool.begin().await.unwrap();
        assert!(add_part(&mut tx, transfer.id, 1).await.is_err());
        drop(tx);

        assert_eq!(take(&pool, bob, &token).await.unwrap(), None);
        let taken = take(&pool, alice, &token).await.unwrap().unwrap();
        assert_eq!(taken.id, transfer.id);
        assert_eq!(take(&pool, alice, &token).await.unwrap(), None);
        assert_eq!(unfinished(&pool, alice).await.unwrap(), 0);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn idle_transfers_are_taken(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let (_, old) = create(&pool, alice, "old.png", 5).await.unwrap();
        let (_, new) = create(&pool, alice, "new.png", 5).await.unwrap();
        sqlx::query(
            "UPDATE file_transfers SET updated_at = now() - interval '2 hours' WHERE id = $1",
        )
        .bind(old.id)
        .execute(&pool)
        .await
        .unwrap();
        let idle = take_idle(&pool, Duration::from_secs(3600), 10)
            .await
            .unwrap();
        assert_eq!(idle.iter().map(|t| t.id).collect::<Vec<_>>(), [old.id]);
        assert!(
            take_idle(&pool, Duration::from_secs(3600), 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(unfinished(&pool, alice).await.unwrap(), 1);
        let _ = new;
    }
}
