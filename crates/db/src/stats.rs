//! Figures for the admin overview.

use sqlx::PgExecutor;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Overview {
    pub active_posts: i64,
    pub pending_posts: i64,
    pub flagged_posts: i64,
    pub deleted_posts: i64,
    pub users: i64,
    pub tags: i64,
    /// Bytes of originals and renditions.
    pub file_bytes: i64,
    pub database_bytes: i64,
}

/// Exact counts; fine on a dashboard viewed now and then, but they scan,
/// so large sites should expect them to take a moment.
pub async fn overview(db: impl PgExecutor<'_>) -> sqlx::Result<Overview> {
    sqlx::query_as(
        "SELECT
             (SELECT count(*) FROM posts WHERE status = 'active') AS active_posts,
             (SELECT count(*) FROM posts WHERE status = 'pending') AS pending_posts,
             (SELECT count(*) FROM posts WHERE status = 'flagged') AS flagged_posts,
             (SELECT count(*) FROM posts WHERE status = 'deleted') AS deleted_posts,
             (SELECT count(*) FROM users) AS users,
             (SELECT count(*) FROM tags WHERE post_count > 0) AS tags,
             (SELECT coalesce(sum(file_size), 0) FROM media_assets)::bigint
               + (SELECT coalesce(sum(file_size), 0) FROM media_variants)::bigint AS file_bytes,
             pg_database_size(current_database()) AS database_bytes",
    )
    .fetch_one(db)
    .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn counts_things(pool: PgPool) {
        sqlx::query("INSERT INTO posts (rating, status) VALUES ('g', 'active'), ('g', 'pending')")
            .execute(&pool)
            .await
            .unwrap();
        let stats = overview(&pool).await.unwrap();
        assert_eq!((stats.active_posts, stats.pending_posts), (1, 1));
        assert!(stats.database_bytes > 0);
    }
}
