//! Replacing a post's file (`post_replacements`): the asset takes the new
//! file's details, loses its renditions until processed again, and each
//! replacement keeps the file before and after.

use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

use crate::media::{Asset, NewAsset};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Replacement {
    pub id: i64,
    pub post_id: i64,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub reason: String,
    pub source: String,
    pub old_md5: Vec<u8>,
    pub old_media_type: String,
    pub old_width: i32,
    pub old_height: i32,
    pub old_file_size: i64,
    pub old_storage_key: String,
    pub new_md5: Vec<u8>,
    pub new_media_type: String,
    pub new_width: i32,
    pub new_height: i32,
    pub new_file_size: i64,
    pub new_storage_key: String,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    #[error("the post has no file")]
    NoFile,
    #[error("that's the file the post has")]
    Same,
    /// Another post has the file.
    #[error("already uploaded as post #{0}")]
    Duplicate(i64),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What a replacement left behind: the file before, and its renditions'
/// storage keys (no longer used, unless the same file comes back).
#[derive(Debug)]
pub struct Replaced {
    pub old: Asset,
    pub old_variant_keys: Vec<String>,
}

/// Makes `new` (its `post_id` names the post) the post's file, recording
/// the replacement. The asset needs processing again; the caller queues
/// it in the same transaction.
pub async fn replace(
    conn: &mut PgConnection,
    new: &NewAsset<'_>,
    creator_id: Option<i64>,
    reason: &str,
    source: &str,
) -> Result<Replaced, ReplaceError> {
    let old: Asset = sqlx::query_as(
        "SELECT id, post_id, sha256, md5, media_type, width, height, duration_ms, frames,
                has_audio, file_size, storage_key, phash, processed_at
         FROM media_assets WHERE post_id = $1 FOR UPDATE",
    )
    .bind(new.post_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(ReplaceError::NoFile)?;
    if old.sha256 == new.sha256[..] {
        return Err(ReplaceError::Same);
    }
    if let Some(other) = crate::media::post_with_sha256(&mut *conn, new.sha256).await? {
        return Err(ReplaceError::Duplicate(other));
    }
    sqlx::query(
        "INSERT INTO post_replacements
             (post_id, creator_id, reason, source,
              old_sha256, old_md5, old_media_type, old_width, old_height, old_file_size, old_storage_key,
              new_sha256, new_md5, new_media_type, new_width, new_height, new_file_size, new_storage_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
    )
    .bind(new.post_id)
    .bind(creator_id)
    .bind(reason)
    .bind(source)
    .bind(&old.sha256)
    .bind(&old.md5)
    .bind(&old.media_type)
    .bind(old.width)
    .bind(old.height)
    .bind(old.file_size)
    .bind(&old.storage_key)
    .bind(&new.sha256[..])
    .bind(&new.md5[..])
    .bind(new.media_type)
    .bind(new.width)
    .bind(new.height)
    .bind(new.file_size)
    .bind(new.storage_key)
    .execute(&mut *conn)
    .await?;
    let updated = sqlx::query(
        "UPDATE media_assets
         SET sha256 = $2, md5 = $3, media_type = $4, width = $5, height = $6, duration_ms = $7,
             frames = $8, has_audio = $9, file_size = $10, storage_key = $11, metadata = '{}',
             phash = NULL, phash_0 = NULL, phash_1 = NULL, phash_2 = NULL, phash_3 = NULL,
             processed_at = NULL
         WHERE id = $1",
    )
    .bind(old.id)
    .bind(&new.sha256[..])
    .bind(&new.md5[..])
    .bind(new.media_type)
    .bind(new.width)
    .bind(new.height)
    .bind(new.duration_ms)
    .bind(new.frames)
    .bind(new.has_audio)
    .bind(new.file_size)
    .bind(new.storage_key)
    .execute(&mut *conn)
    .await;
    if let Err(error) = updated {
        return Err(match crate::users::unique_violation(&error) {
            Some("media_assets_sha256_key") => ReplaceError::Duplicate(0),
            _ => ReplaceError::Db(error),
        });
    }
    let old_variant_keys: Vec<String> =
        sqlx::query_scalar("DELETE FROM media_variants WHERE asset_id = $1 RETURNING storage_key")
            .bind(old.id)
            .fetch_all(&mut *conn)
            .await?;
    // The post counts as changed.
    sqlx::query("UPDATE posts SET updated_at = now() WHERE id = $1")
        .bind(new.post_id)
        .execute(&mut *conn)
        .await?;
    Ok(Replaced {
        old,
        old_variant_keys,
    })
}

/// Which replacements [`list`] returns.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter {
    pub post_id: Option<i64>,
    pub creator_id: Option<i64>,
}

/// Replacements, newest first.
pub async fn list(
    db: impl PgExecutor<'_>,
    filter: Filter,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Replacement>> {
    sqlx::query_as(
        "SELECT r.id, r.post_id, r.creator_id, u.name::text AS creator_name, r.reason, r.source,
                r.old_md5, r.old_media_type, r.old_width, r.old_height, r.old_file_size,
                r.old_storage_key, r.new_md5, r.new_media_type, r.new_width, r.new_height,
                r.new_file_size, r.new_storage_key, r.created_at
         FROM post_replacements r LEFT JOIN users u ON u.id = r.creator_id
         WHERE ($1::bigint IS NULL OR r.post_id = $1) AND ($2::bigint IS NULL OR r.creator_id = $2)
         ORDER BY r.id DESC OFFSET $3 LIMIT $4",
    )
    .bind(filter.post_id)
    .bind(filter.creator_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The files post `post_id` had before, which nothing else uses: for
/// removing when the post is purged.
pub async fn old_keys(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT DISTINCT r.old_storage_key FROM post_replacements r
         WHERE r.post_id = $1
           AND NOT EXISTS (SELECT 1 FROM media_assets a WHERE a.storage_key = r.old_storage_key)",
    )
    .bind(post_id)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    async fn asset(pool: &PgPool, byte: u8) -> i64 {
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
        let key = format!("original/{byte:02x}/00/{byte:02x}00.png");
        crate::media::insert(
            pool,
            NewAsset {
                post_id: post,
                sha256: &[byte; 32],
                md5: &[byte; 16],
                media_type: "png",
                width: 10,
                height: 10,
                duration_ms: None,
                frames: 1,
                has_audio: false,
                file_size: 5,
                storage_key: &key,
            },
        )
        .await
        .unwrap();
        post
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn replaces_and_records(pool: PgPool) {
        let post = asset(&pool, 1).await;
        let other = asset(&pool, 2).await;
        let new = |sha: u8| NewAsset {
            post_id: post,
            sha256: Box::leak(Box::new([sha; 32])),
            md5: Box::leak(Box::new([sha; 16])),
            media_type: "jpeg",
            width: 20,
            height: 40,
            duration_ms: None,
            frames: 1,
            has_audio: false,
            file_size: 9,
            storage_key: "original/03/00/0300.jpg",
        };
        let mut conn = pool.acquire().await.unwrap();
        assert!(matches!(
            replace(&mut conn, &new(1), None, "", "").await,
            Err(ReplaceError::Same)
        ));
        assert!(matches!(
            replace(&mut conn, &new(2), None, "", "").await,
            Err(ReplaceError::Duplicate(p)) if p == other
        ));
        let done = replace(&mut conn, &new(3), None, "better version", "https://x")
            .await
            .unwrap();
        assert_eq!(done.old.width, 10);
        let now = crate::media::for_post(&pool, post).await.unwrap().unwrap();
        assert_eq!(
            (now.width, now.height, now.media_type.as_str()),
            (20, 40, "jpeg")
        );
        assert!(now.processed_at.is_none());
        let listed = list(
            &pool,
            Filter {
                post_id: Some(post),
                ..Filter::default()
            },
            0,
            10,
        )
        .await
        .unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].reason, "better version");
        assert_eq!(listed[0].old_storage_key, "original/01/00/0100.png");
        assert_eq!(
            old_keys(&pool, post).await.unwrap(),
            ["original/01/00/0100.png"]
        );
    }
}
