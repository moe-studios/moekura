//! Appeals: requests to bring back a deleted post.

use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Appeal {
    pub id: i64,
    pub post_id: i64,
    pub creator_name: Option<String>,
    pub reason: String,
    pub status: String,
    pub resolver_name: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, thiserror::Error)]
pub enum AppealError {
    #[error("This post has already been appealed; staff will look at it.")]
    AlreadyAppealed,
    #[error("Only deleted posts can be appealed.")]
    NotDeleted,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Appeals deleted post `post_id`; returns the appeal's id.
pub async fn create(
    conn: &mut PgConnection,
    post_id: i64,
    creator_id: i64,
    reason: &str,
) -> Result<i64, AppealError> {
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM posts WHERE id = $1 FOR UPDATE")
            .bind(post_id)
            .fetch_optional(&mut *conn)
            .await?;
    if status.as_deref() != Some("deleted") {
        return Err(AppealError::NotDeleted);
    }
    sqlx::query_scalar(
        "INSERT INTO post_appeals (post_id, creator_id, reason) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(post_id)
    .bind(creator_id)
    .bind(reason)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => AppealError::AlreadyAppealed,
        _ => AppealError::Db(e),
    })
}

/// Closes a post's open appeal as `approved` (the post was restored) or
/// `rejected`; returns how many there were (0 or 1).
pub async fn resolve(
    db: impl PgExecutor<'_>,
    post_id: i64,
    approved: bool,
    resolver_id: Option<i64>,
) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE post_appeals SET status = $2, resolver_id = $3, resolved_at = now()
         WHERE post_id = $1 AND status = 'open'",
    )
    .bind(post_id)
    .bind(if approved { "approved" } else { "rejected" })
    .bind(resolver_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// `SELECT <appeal columns> …` followed by `$rest`.
macro_rules! select_appeals {
    ($rest:literal) => {
        concat!(
            "SELECT a.id, a.post_id, c.name::text AS creator_name, a.reason, a.status,
                    r.name::text AS resolver_name, a.created_at
             FROM post_appeals a
             LEFT JOIN users c ON c.id = a.creator_id
             LEFT JOIN users r ON r.id = a.resolver_id ",
            $rest
        )
    };
}

/// A post's appeals, oldest first.
pub async fn for_post(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Vec<Appeal>> {
    sqlx::query_as(select_appeals!("WHERE a.post_id = $1 ORDER BY a.id"))
        .bind(post_id)
        .fetch_all(db)
        .await
}

/// Open appeals, oldest first, after appeal `after` (0 for the first).
pub async fn open(db: impl PgExecutor<'_>, after: i64, limit: i64) -> sqlx::Result<Vec<Appeal>> {
    sqlx::query_as(select_appeals!(
        "WHERE a.status = 'open' AND a.id > $1 ORDER BY a.id LIMIT $2"
    ))
    .bind(after)
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
    async fn appealing_and_deciding(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let deleted: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, status) VALUES ('g', 'deleted') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let active: i64 =
            sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        assert!(matches!(
            create(&mut conn, active, alice, "x").await,
            Err(AppealError::NotDeleted)
        ));
        create(&mut conn, deleted, alice, "it's on topic")
            .await
            .unwrap();
        assert!(matches!(
            create(&mut conn, deleted, alice, "again").await,
            Err(AppealError::AlreadyAppealed)
        ));
        assert_eq!(open(&pool, 0, 10).await.unwrap().len(), 1);
        assert_eq!(resolve(&pool, deleted, false, None).await.unwrap(), 1);
        assert!(open(&pool, 0, 10).await.unwrap().is_empty());
        // A rejected appeal can be followed by another.
        create(&mut conn, deleted, alice, "please").await.unwrap();
        let history = for_post(&pool, deleted).await.unwrap();
        assert_eq!(
            history
                .iter()
                .map(|a| a.status.as_str())
                .collect::<Vec<_>>(),
            ["rejected", "open"]
        );
        assert_eq!(history[0].creator_name.as_deref(), Some("alice"));
    }
}
