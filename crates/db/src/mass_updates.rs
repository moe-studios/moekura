//! Mass tag edits (`mass_updates`) and changing tags on many posts.

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct MassUpdate {
    pub id: i64,
    pub creator_name: Option<String>,
    pub creator_id: Option<i64>,
    pub query: String,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<String>,
    pub status: String,
    pub seen: i32,
    pub changed: i32,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

macro_rules! select_updates {
    ($rest:literal) => {
        concat!(
            "SELECT m.id, u.name::text AS creator_name, m.creator_id, m.query, m.add_tags,
                    m.remove_tags, m.status, m.seen, m.changed, m.error, m.created_at,
                    m.finished_at
             FROM mass_updates m LEFT JOIN users u ON u.id = m.creator_id ",
            $rest
        )
    };
}

/// Records a mass edit to run; returns its id.
pub async fn create(
    db: impl PgExecutor<'_>,
    creator_id: Option<i64>,
    query: &str,
    add: &[String],
    remove: &[String],
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO mass_updates (creator_id, query, add_tags, remove_tags)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(creator_id)
    .bind(query)
    .bind(add)
    .bind(remove)
    .fetch_one(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<MassUpdate>> {
    sqlx::query_as(select_updates!("WHERE m.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The latest mass edits, newest first.
pub async fn recent(db: impl PgExecutor<'_>, limit: i64) -> sqlx::Result<Vec<MassUpdate>> {
    sqlx::query_as(select_updates!("ORDER BY m.id DESC LIMIT $1"))
        .bind(limit)
        .fetch_all(db)
        .await
}

/// Marks a mass edit running, starting its counts over (a retried job
/// looks at every post again; changing one twice changes nothing).
pub async fn start(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE mass_updates SET status = 'running', seen = 0, changed = 0, error = NULL
         WHERE id = $1",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn progress(
    db: impl PgExecutor<'_>,
    id: i64,
    seen: i32,
    changed: i32,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE mass_updates SET seen = $2, changed = $3 WHERE id = $1")
        .bind(id)
        .bind(seen)
        .bind(changed)
        .execute(db)
        .await?;
    Ok(())
}

/// Marks a mass edit done, or failed with `error`.
pub async fn finish(db: impl PgExecutor<'_>, id: i64, error: Option<&str>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE mass_updates SET status = CASE WHEN $2::text IS NULL THEN 'done' ELSE 'failed' END,
                                 error = $2, finished_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

/// Adds tags `add` and takes off `remove` on posts `ids`, credited to
/// `updater_id` in their history. Returns how many posts changed.
pub async fn retag(
    db: &PgPool,
    ids: &[i64],
    add: &[i32],
    remove: &[i32],
    updater_id: Option<i64>,
) -> sqlx::Result<u64> {
    let mut tx = db.begin().await?;
    crate::post_versions::attribute(&mut tx, updater_id, None).await?;
    let result = sqlx::query(
        "WITH batch AS (SELECT id FROM posts WHERE id = ANY($1) ORDER BY id FOR UPDATE)
         UPDATE posts SET tag_ids = uniq(sort((posts.tag_ids - $3::int4[]) | $2::int4[])),
                          updated_at = now()
         FROM batch
         WHERE posts.id = batch.id
           AND posts.tag_ids <> uniq(sort((posts.tag_ids - $3::int4[]) | $2::int4[]))",
    )
    .bind(ids)
    .bind(add)
    .bind(remove)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}
