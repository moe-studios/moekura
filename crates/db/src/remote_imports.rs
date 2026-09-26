//! Where imports from other boorus got to, and which local posts their
//! posts became.

use moekura_core::remote::Cursor;
use sqlx::{PgExecutor, PgPool};

/// An import's progress.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    pub cursor: Option<Cursor>,
    pub imported: i32,
    pub duplicates: i32,
    pub failed: i32,
    pub finished: bool,
}

fn cursor_text(cursor: Cursor) -> String {
    match cursor {
        Cursor::Start => "start".into(),
        Cursor::Before(id) => format!("b{id}"),
        Cursor::Page(n) => format!("p{n}"),
    }
}

fn parse_cursor(text: &str) -> Cursor {
    if let Some(id) = text.strip_prefix('b').and_then(|id| id.parse().ok()) {
        return Cursor::Before(id);
    }
    if let Some(n) = text.strip_prefix('p').and_then(|n| n.parse().ok()) {
        return Cursor::Page(n);
    }
    Cursor::Start
}

/// Where the import of `query` from `site` got to, if it was started.
pub async fn state(
    db: impl PgExecutor<'_>,
    site: &str,
    query: &str,
) -> sqlx::Result<Option<State>> {
    let row: Option<(String, i32, i32, i32, bool)> = sqlx::query_as(
        "SELECT cursor, imported, duplicates, failed, finished FROM remote_imports
         WHERE site = $1 AND query = $2",
    )
    .bind(site)
    .bind(query)
    .fetch_optional(db)
    .await?;
    Ok(
        row.map(|(cursor, imported, duplicates, failed, finished)| State {
            cursor: Some(parse_cursor(&cursor)),
            imported,
            duplicates,
            failed,
            finished,
        }),
    )
}

pub async fn save(
    db: impl PgExecutor<'_>,
    site: &str,
    query: &str,
    state: &State,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO remote_imports (site, query, cursor, imported, duplicates, failed, finished)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (site, query) DO UPDATE
         SET cursor = EXCLUDED.cursor, imported = EXCLUDED.imported,
             duplicates = EXCLUDED.duplicates, failed = EXCLUDED.failed,
             finished = EXCLUDED.finished, updated_at = now()",
    )
    .bind(site)
    .bind(query)
    .bind(cursor_text(state.cursor.unwrap_or(Cursor::Start)))
    .bind(state.imported)
    .bind(state.duplicates)
    .bind(state.failed)
    .bind(state.finished)
    .execute(db)
    .await?;
    Ok(())
}

/// The local post a remote post became.
pub async fn local_post(
    db: impl PgExecutor<'_>,
    site: &str,
    remote_id: i64,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT post_id FROM remote_posts WHERE site = $1 AND remote_id = $2")
        .bind(site)
        .bind(remote_id)
        .fetch_optional(db)
        .await
}

/// Records that remote post `remote_id` became `post_id`, and links it
/// with its parent and children among the posts imported from `site`.
/// Changes are credited to `updater_id` in post history.
pub async fn record(
    db: &PgPool,
    site: &str,
    remote_id: i64,
    post_id: i64,
    remote_parent_id: Option<i64>,
    updater_id: Option<i64>,
) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "INSERT INTO remote_posts (site, remote_id, post_id, remote_parent_id) VALUES ($1, $2, $3, $4)
         ON CONFLICT (site, remote_id) DO UPDATE
         SET post_id = EXCLUDED.post_id, remote_parent_id = EXCLUDED.remote_parent_id",
    )
    .bind(site)
    .bind(remote_id)
    .bind(post_id)
    .bind(remote_parent_id)
    .execute(&mut *tx)
    .await?;
    crate::post_versions::attribute(&mut tx, updater_id, None).await?;
    // Its parent, if already here.
    sqlx::query(
        "UPDATE posts SET parent_id = r.post_id, updated_at = now()
         FROM remote_posts r
         WHERE posts.id = $3 AND posts.parent_id IS NULL
           AND r.site = $1 AND r.remote_id = $2 AND r.post_id <> posts.id",
    )
    .bind(site)
    .bind(remote_parent_id)
    .bind(post_id)
    .execute(&mut *tx)
    .await?;
    // Its children, imported before it.
    sqlx::query(
        "UPDATE posts SET parent_id = $3, updated_at = now()
         FROM remote_posts r
         WHERE posts.id = r.post_id AND posts.parent_id IS NULL AND posts.id <> $3
           AND r.site = $1 AND r.remote_parent_id = $2",
    )
    .bind(site)
    .bind(remote_id)
    .bind(post_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn post(pool: &PgPool) -> i64 {
        sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn parent_of(pool: &PgPool, id: i64) -> Option<i64> {
        sqlx::query_scalar("SELECT parent_id FROM posts WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn state_and_links(pool: PgPool) {
        let site = "https://d.example";
        assert_eq!(state(&pool, site, "cat").await.unwrap(), None);
        let saved = State {
            cursor: Some(Cursor::Before(40)),
            imported: 3,
            duplicates: 1,
            failed: 0,
            finished: false,
        };
        save(&pool, site, "cat", &saved).await.unwrap();
        assert_eq!(state(&pool, site, "cat").await.unwrap(), Some(saved));

        // A child arrives before its parent, then another after.
        let child = post(&pool).await;
        record(&pool, site, 11, child, Some(10), None)
            .await
            .unwrap();
        assert_eq!(parent_of(&pool, child).await, None);
        let parent = post(&pool).await;
        record(&pool, site, 10, parent, None, None).await.unwrap();
        assert_eq!(parent_of(&pool, child).await, Some(parent));
        let later = post(&pool).await;
        record(&pool, site, 12, later, Some(10), None)
            .await
            .unwrap();
        assert_eq!(parent_of(&pool, later).await, Some(parent));
        assert_eq!(local_post(&pool, site, 10).await.unwrap(), Some(parent));
        assert_eq!(local_post(&pool, "other", 10).await.unwrap(), None);
    }
}
