//! Files uploaded but not yet made into posts (`staged_uploads`).

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Staged {
    pub id: i64,
    pub uploader_id: i64,
    pub source: String,
    pub sha256: Vec<u8>,
    pub md5: Vec<u8>,
    pub media_type: String,
    pub width: i32,
    pub height: i32,
    pub duration_ms: Option<i32>,
    pub frames: i32,
    pub has_audio: bool,
    pub file_size: i64,
    pub storage_key: String,
    pub post_id: Option<i64>,
    pub created_at: OffsetDateTime,
}

/// A new staged upload.
#[derive(Debug, Clone)]
pub struct NewStaged<'a> {
    pub uploader_id: i64,
    pub source: &'a str,
    pub sha256: &'a [u8],
    pub md5: &'a [u8],
    pub media_type: &'a str,
    pub width: i32,
    pub height: i32,
    pub duration_ms: Option<i32>,
    pub frames: i32,
    pub has_audio: bool,
    pub file_size: i64,
    pub storage_key: &'a str,
}

pub async fn create(db: impl PgExecutor<'_>, new: NewStaged<'_>) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO staged_uploads (uploader_id, source, sha256, md5, media_type, width, height,
                                     duration_ms, frames, has_audio, file_size, storage_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
    )
    .bind(new.uploader_id)
    .bind(new.source)
    .bind(new.sha256)
    .bind(new.md5)
    .bind(new.media_type)
    .bind(new.width)
    .bind(new.height)
    .bind(new.duration_ms)
    .bind(new.frames)
    .bind(new.has_audio)
    .bind(new.file_size)
    .bind(new.storage_key)
    .fetch_one(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Staged>> {
    sqlx::query_as("SELECT * FROM staged_uploads WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Records the post a staged upload became; false if it already became
/// one.
pub async fn used(db: impl PgExecutor<'_>, id: i64, post_id: i64) -> sqlx::Result<bool> {
    let result =
        sqlx::query("UPDATE staged_uploads SET post_id = $2 WHERE id = $1 AND post_id IS NULL")
            .bind(id)
            .bind(post_id)
            .execute(db)
            .await?;
    Ok(result.rows_affected() == 1)
}

/// Removes staged uploads unused for longer than `max_age`, and returns
/// the storage keys no post or other staged upload uses any more, for
/// the caller to delete.
pub async fn expire(db: &PgPool, max_age: std::time::Duration) -> sqlx::Result<Vec<String>> {
    let keys: Vec<String> = sqlx::query_scalar(
        "WITH gone AS (
             DELETE FROM staged_uploads
             WHERE post_id IS NULL AND created_at < now() - make_interval(secs => $1)
             RETURNING id, storage_key
         )
         -- The statement still sees the rows it deletes, so they're left
         -- out by id.
         SELECT DISTINCT g.storage_key FROM gone g
         WHERE NOT EXISTS (SELECT 1 FROM media_assets a WHERE a.storage_key = g.storage_key)
           AND NOT EXISTS (SELECT 1 FROM staged_uploads s
                           WHERE s.storage_key = g.storage_key
                             AND s.id NOT IN (SELECT id FROM gone))",
    )
    .bind(max_age.as_secs_f64())
    .fetch_all(db)
    .await?;
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn stages_uses_and_expires(pool: PgPool) {
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let new = |key: &'static str| NewStaged {
            uploader_id: user,
            source: "",
            sha256: &[1; 32],
            md5: &[1; 16],
            media_type: "png",
            width: 1,
            height: 1,
            duration_ms: None,
            frames: 1,
            has_audio: false,
            file_size: 1,
            storage_key: key,
        };
        let used_one = create(&pool, new("original/a.png")).await.unwrap();
        let unused = create(&pool, new("original/b.png")).await.unwrap();
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(used(&pool, used_one, post).await.unwrap());
        assert!(!used(&pool, used_one, post).await.unwrap());
        assert!(
            expire(&pool, std::time::Duration::from_secs(3600))
                .await
                .unwrap()
                .is_empty()
        );
        sqlx::query("UPDATE staged_uploads SET created_at = now() - interval '2 days'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            expire(&pool, std::time::Duration::from_secs(3600))
                .await
                .unwrap(),
            ["original/b.png"]
        );
        assert!(by_id(&pool, unused).await.unwrap().is_none());
        assert!(by_id(&pool, used_one).await.unwrap().is_some());
    }
}
