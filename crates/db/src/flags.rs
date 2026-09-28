//! Post flags: reports that a post breaks the rules.

use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Flag {
    pub id: i64,
    pub post_id: i64,
    pub creator_name: Option<String>,
    pub reason: String,
    pub status: String,
    pub resolver_name: Option<String>,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, thiserror::Error)]
pub enum FlagError {
    #[error("You already flagged this post.")]
    AlreadyFlagged,
    #[error("Only active posts can be flagged.")]
    NotActive,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Flags a post, marking it flagged if it was active.
pub async fn create(
    conn: &mut PgConnection,
    post_id: i64,
    creator_id: i64,
    reason: &str,
) -> Result<(), FlagError> {
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM posts WHERE id = $1 FOR UPDATE")
            .bind(post_id)
            .fetch_optional(&mut *conn)
            .await?;
    if !matches!(status.as_deref(), Some("active" | "flagged")) {
        return Err(FlagError::NotActive);
    }
    sqlx::query("INSERT INTO post_flags (post_id, creator_id, reason) VALUES ($1, $2, $3)")
        .bind(post_id)
        .bind(creator_id)
        .bind(reason)
        .execute(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => FlagError::AlreadyFlagged,
            _ => FlagError::Db(e),
        })?;
    sqlx::query("UPDATE posts SET status = 'flagged' WHERE id = $1 AND status = 'active'")
        .bind(post_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Closes a post's open flags as `dismissed` or `upheld`; returns how
/// many there were.
pub async fn resolve(
    db: impl PgExecutor<'_>,
    post_id: i64,
    upheld: bool,
    resolver_id: Option<i64>,
) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE post_flags SET status = $2, resolver_id = $3, resolved_at = now()
         WHERE post_id = $1 AND status = 'open'",
    )
    .bind(post_id)
    .bind(if upheld { "upheld" } else { "dismissed" })
    .bind(resolver_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// `SELECT <flag columns> …` followed by `$rest`.
macro_rules! select_flags {
    ($rest:literal) => {
        concat!(
            "SELECT f.id, f.post_id, c.name::text AS creator_name, f.reason, f.status,
                    r.name::text AS resolver_name, f.created_at
             FROM post_flags f
             LEFT JOIN users c ON c.id = f.creator_id
             LEFT JOIN users r ON r.id = f.resolver_id ",
            $rest
        )
    };
}

/// A post's flags, oldest first.
pub async fn for_post(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Vec<Flag>> {
    sqlx::query_as(select_flags!("WHERE f.post_id = $1 ORDER BY f.id"))
        .bind(post_id)
        .fetch_all(db)
        .await
}

/// Open flags on up to `limit` posts, the post whose flags have waited
/// longest first, each post's flags together and oldest first. Pages go
/// by the id of a post's oldest open flag: pass the last post's as
/// `after` for the next page (0 for the first).
pub async fn open(db: impl PgExecutor<'_>, after: i64, limit: i64) -> sqlx::Result<Vec<Flag>> {
    sqlx::query_as(
        "WITH queued AS (
             SELECT post_id, min(id) AS first FROM post_flags WHERE status = 'open'
             GROUP BY post_id HAVING min(id) > $1 ORDER BY first LIMIT $2)
         SELECT f.id, f.post_id, c.name::text AS creator_name, f.reason, f.status,
                r.name::text AS resolver_name, f.created_at
         FROM queued q JOIN post_flags f ON f.post_id = q.post_id AND f.status = 'open'
         LEFT JOIN users c ON c.id = f.creator_id
         LEFT JOIN users r ON r.id = f.resolver_id
         ORDER BY q.first, f.id",
    )
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

    async fn status(pool: &PgPool, post: i64) -> String {
        sqlx::query_scalar("SELECT status FROM posts WHERE id = $1")
            .bind(post)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn flagging_and_resolving(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        create(&mut conn, post, alice, "off-topic").await.unwrap();
        assert!(matches!(
            create(&mut conn, post, alice, "again").await,
            Err(FlagError::AlreadyFlagged)
        ));
        create(&mut conn, post, bob, "low quality").await.unwrap();
        assert_eq!(status(&pool, post).await, "flagged");
        assert_eq!(open(&pool, 0, 10).await.unwrap().len(), 2);

        assert_eq!(resolve(&pool, post, false, Some(bob)).await.unwrap(), 2);
        assert!(open(&pool, 0, 10).await.unwrap().is_empty());
        let history = for_post(&pool, post).await.unwrap();
        assert_eq!(history[0].status, "dismissed");
        assert_eq!(history[0].resolver_name.as_deref(), Some("bob"));
        // Flagging again after a dismissal is allowed.
        create(&mut conn, post, alice, "still off-topic")
            .await
            .unwrap();

        sqlx::query("UPDATE posts SET status = 'deleted' WHERE id = $1")
            .bind(post)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            create(&mut conn, post, bob, "x").await,
            Err(FlagError::NotActive)
        ));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn the_queue_pages_by_oldest_flag(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let mut posts = Vec::new();
        for _ in 0..3 {
            posts.push(
                sqlx::query_scalar::<_, i64>(
                    "INSERT INTO posts (rating) VALUES ('g') RETURNING id",
                )
                .fetch_one(&pool)
                .await
                .unwrap(),
            );
        }
        let mut conn = pool.acquire().await.unwrap();
        // Flagged in the order 2, 0, 1; post 2 twice.
        for (post, by) in [(2, alice), (0, alice), (2, bob), (1, alice)] {
            create(&mut conn, posts[post], by, "x").await.unwrap();
        }
        let first = open(&pool, 0, 2).await.unwrap();
        assert_eq!(
            first.iter().map(|f| f.post_id).collect::<Vec<_>>(),
            [posts[2], posts[2], posts[0]]
        );
        let after = first.iter().find(|f| f.post_id == posts[0]).unwrap().id;
        let next = open(&pool, after, 2).await.unwrap();
        assert_eq!(
            next.iter().map(|f| f.post_id).collect::<Vec<_>>(),
            [posts[1]]
        );
    }
}
