//! Queries on `tags` and `tag_categories`.

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Category {
    pub id: i16,
    pub name: String,
    pub label: String,
    pub position: i16,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Tag {
    pub id: i32,
    pub name: String,
    pub category_id: i16,
    pub post_count: i32,
    pub is_deprecated: bool,
    pub created_at: OffsetDateTime,
}

/// `SELECT <tag columns> FROM tags` followed by `$rest`.
macro_rules! select_tags {
    ($rest:literal) => {
        concat!(
            "SELECT id, name, category_id, post_count, is_deprecated, created_at FROM tags ",
            $rest
        )
    };
}

/// All categories, in display order.
pub async fn categories(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<Category>> {
    sqlx::query_as("SELECT id, name, label, position FROM tag_categories ORDER BY position, id")
        .fetch_all(db)
        .await
}

pub async fn by_name(db: impl PgExecutor<'_>, name: &str) -> sqlx::Result<Option<Tag>> {
    sqlx::query_as(select_tags!("WHERE name = $1"))
        .bind(name)
        .fetch_optional(db)
        .await
}

/// The tags among `names` that exist, in no particular order.
pub async fn by_names(db: impl PgExecutor<'_>, names: &[&str]) -> sqlx::Result<Vec<Tag>> {
    sqlx::query_as(select_tags!("WHERE name = ANY($1)"))
        .bind(names)
        .fetch_all(db)
        .await
}

/// The tags with the given ids, most used first.
pub async fn by_ids(db: impl PgExecutor<'_>, ids: &[i32]) -> sqlx::Result<Vec<Tag>> {
    sqlx::query_as(select_tags!(
        "WHERE id = ANY($1) ORDER BY post_count DESC, name"
    ))
    .bind(ids)
    .fetch_all(db)
    .await
}

/// Recomputes every tag's post count from the posts, returning how many
/// were wrong. Blocks post edits while it runs (a full scan of `posts`), so
/// counts can't drift during the recount.
pub async fn recount(db: &PgPool) -> sqlx::Result<u64> {
    let mut tx = db.begin().await?;
    sqlx::query("LOCK TABLE posts IN SHARE MODE")
        .execute(&mut *tx)
        .await?;
    let fixed = sqlx::query(
        "WITH counts AS (
             SELECT tag_id, count(*)::integer AS n
             FROM posts, unnest(tag_ids) AS tag_id
             WHERE status IN ('active', 'flagged')
             GROUP BY tag_id
         )
         UPDATE tags SET post_count = coalesce(counts.n, 0)
         FROM tags AS t LEFT JOIN counts ON counts.tag_id = t.id
         WHERE tags.id = t.id AND tags.post_count IS DISTINCT FROM coalesce(counts.n, 0)",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(fixed)
}

#[cfg(test)]
pub(crate) mod tests {
    use sqlx::PgPool;

    use super::*;

    /// Creates tags by name, returning their ids in the same order.
    pub(crate) async fn create(pool: &PgPool, names: &[&str]) -> Vec<i32> {
        let mut ids = Vec::new();
        for name in names {
            ids.push(
                sqlx::query_scalar("INSERT INTO tags (name) VALUES ($1) RETURNING id")
                    .bind(name)
                    .fetch_one(pool)
                    .await
                    .unwrap(),
            );
        }
        ids
    }

    async fn counts(pool: &PgPool, ids: &[i32]) -> Vec<i32> {
        let mut out = Vec::new();
        for id in ids {
            out.push(
                sqlx::query_scalar("SELECT post_count FROM tags WHERE id = $1")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap(),
            );
        }
        out
    }

    async fn post(pool: &PgPool, status: &str, tags: &[i32]) -> i64 {
        let mut tags = tags.to_vec();
        tags.sort_unstable();
        sqlx::query_scalar(
            "INSERT INTO posts (rating, status, tag_ids) VALUES ('g', $1, $2) RETURNING id",
        )
        .bind(status)
        .bind(tags)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn default_categories(pool: PgPool) {
        let names: Vec<String> = categories(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            ["artist", "copyright", "character", "general", "meta"]
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn counts_follow_tag_and_status_changes(pool: PgPool) {
        let [a, b, c] = create(&pool, &["a", "b", "c"]).await[..] else {
            unreachable!()
        };
        let p1 = post(&pool, "active", &[a, b]).await;
        let p2 = post(&pool, "flagged", &[a]).await;
        post(&pool, "pending", &[a, b, c]).await;
        assert_eq!(counts(&pool, &[a, b, c]).await, [2, 1, 0]);

        sqlx::query("UPDATE posts SET tag_ids = $2 WHERE id = $1")
            .bind(p1)
            .bind(vec![b, c])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, &[a, b, c]).await, [1, 1, 1]);

        // Deleting a post (soft or hard) removes its tags from the counts;
        // restoring adds them back.
        for status in ["deleted", "active"] {
            sqlx::query("UPDATE posts SET status = $2 WHERE id = $1")
                .bind(p1)
                .bind(status)
                .execute(&pool)
                .await
                .unwrap();
        }
        assert_eq!(counts(&pool, &[a, b, c]).await, [1, 1, 1]);
        sqlx::query("UPDATE posts SET status = 'deleted' WHERE id = $1")
            .bind(p1)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM posts WHERE id = $1")
            .bind(p2)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, &[a, b, c]).await, [0, 0, 0]);

        // Unrelated edits don't touch tags.
        sqlx::query("UPDATE posts SET score = 5 WHERE id = $1")
            .bind(p1)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, &[a, b, c]).await, [0, 0, 0]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn concurrent_tagging_keeps_exact_counts(pool: PgPool) {
        let ids = create(&pool, &["a", "b", "c", "d"]).await;
        let tasks: Vec<_> = (0..16)
            .map(|task| {
                let (pool, ids) = (pool.clone(), ids.clone());
                tokio::spawn(async move {
                    for i in 0..10 {
                        // Overlapping subsets, so transactions contend for
                        // the same tag rows.
                        let subset: Vec<i32> = ids
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| (task + i + j) % 3 != 0)
                            .map(|(_, id)| *id)
                            .collect();
                        post(&pool, "active", &subset).await;
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        let before = counts(&pool, &ids).await;
        assert_eq!(recount(&pool).await.unwrap(), 0, "{before:?}");
        assert!(before.iter().all(|&n| n > 0));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn tag_arrays_must_be_sorted_and_unique(pool: PgPool) {
        for bad in [vec![2, 1], vec![1, 1]] {
            let result = sqlx::query("INSERT INTO posts (rating, tag_ids) VALUES ('g', $1)")
                .bind(bad)
                .execute(&pool)
                .await;
            assert!(result.is_err());
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn recount_repairs_drift(pool: PgPool) {
        let ids = create(&pool, &["a", "b"]).await;
        post(&pool, "active", &ids).await;
        post(&pool, "active", &ids[..1]).await;
        sqlx::query("UPDATE tags SET post_count = 99")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(recount(&pool).await.unwrap(), 2);
        assert_eq!(counts(&pool, &ids).await, [2, 1]);
        assert_eq!(recount(&pool).await.unwrap(), 0);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn lookups(pool: PgPool) {
        let ids = create(&pool, &["common", "rare"]).await;
        post(&pool, "active", &ids).await;
        post(&pool, "active", &ids[..1]).await;
        assert_eq!(by_name(&pool, "rare").await.unwrap().unwrap().id, ids[1]);
        assert!(by_name(&pool, "missing").await.unwrap().is_none());
        let found = by_names(&pool, &["rare", "missing", "common"])
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        let ordered: Vec<String> = by_ids(&pool, &[ids[1], ids[0]])
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(ordered, ["common", "rare"]);
    }
}
