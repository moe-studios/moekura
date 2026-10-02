//! Uploads (`uploads`) and their files waiting to be made into posts
//! (`staged_uploads`), as Danbooru's uploads and upload media assets.
//!
//! A file is `ready` once stored, with what making its post needs;
//! `pending` while it's still to be downloaded from `file_url`; and
//! `failed` (with an `error`) if it couldn't be either.

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Status {
    Pending,
    Ready,
    Failed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Ready => "ready",
            Status::Failed => "failed",
        }
    }
}

/// Files sent together, or found at a link.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Upload {
    pub id: i64,
    pub uploader_id: i64,
    /// The link uploaded from, if any.
    pub source: String,
    /// The page the link was found on (sent by the bookmarklet), if any.
    pub referer_url: String,
    pub created_at: OffsetDateTime,
}

/// One file of an upload. The file's details are there when it's
/// [`Status::Ready`].
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Staged {
    pub id: i64,
    pub upload_id: i64,
    pub uploader_id: i64,
    pub position: i32,
    /// The name of the file sent, or the link it came from.
    pub file_name: String,
    /// What the post's source should be.
    pub source: String,
    pub status: Status,
    pub file_url: Option<String>,
    pub error: Option<String>,
    pub duplicate_of: Option<i64>,
    pub sha256: Option<Vec<u8>>,
    pub md5: Option<Vec<u8>>,
    pub media_type: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_ms: Option<i32>,
    pub frames: Option<i32>,
    pub has_audio: Option<bool>,
    pub file_size: Option<i64>,
    pub storage_key: Option<String>,
    pub phash: Option<i64>,
    /// The MD5 of the decoded pixels, for still images.
    pub pixel_hash: Option<Vec<u8>>,
    /// What the file's metadata says
    /// ([`moekura_core::file_traits::FileTrait`]s, as stored).
    pub traits: Vec<String>,
    pub post_id: Option<i64>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A stored file's details.
#[derive(Debug, Clone)]
pub struct StoredFile<'a> {
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
    pub phash: Option<i64>,
    pub pixel_hash: Option<&'a [u8]>,
    pub traits: &'a [String],
}

/// Where a new file goes in an upload, and what it is.
#[derive(Debug, Clone)]
pub struct Slot<'a> {
    pub upload_id: i64,
    pub uploader_id: i64,
    pub position: i32,
    pub file_name: &'a str,
    pub source: &'a str,
}

/// Makes an upload from link `source` (empty for files sent), found on
/// page `referer_url` (empty when unknown).
pub async fn create_upload(
    db: impl PgExecutor<'_>,
    uploader_id: i64,
    source: &str,
    referer_url: &str,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO uploads (uploader_id, source, referer_url) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(uploader_id)
    .bind(source)
    .bind(referer_url)
    .fetch_one(db)
    .await
}

pub async fn upload_by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Upload>> {
    sqlx::query_as("SELECT * FROM uploads WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// User `uploader_id`'s uploads, newest first, from `offset`.
pub async fn uploads_by_uploader(
    db: impl PgExecutor<'_>,
    uploader_id: i64,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Upload>> {
    sqlx::query_as(
        "SELECT * FROM uploads WHERE uploader_id = $1 ORDER BY id DESC OFFSET $2 LIMIT $3",
    )
    .bind(uploader_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Adds a stored file to an upload.
pub async fn create(
    db: impl PgExecutor<'_>,
    slot: Slot<'_>,
    file: StoredFile<'_>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO staged_uploads (upload_id, uploader_id, position, file_name, source,
                                     sha256, md5, media_type, width, height, duration_ms,
                                     frames, has_audio, file_size, storage_key, phash,
                                     pixel_hash, traits)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 $17, $18)
         RETURNING id",
    )
    .bind(slot.upload_id)
    .bind(slot.uploader_id)
    .bind(slot.position)
    .bind(slot.file_name)
    .bind(slot.source)
    .bind(file.sha256)
    .bind(file.md5)
    .bind(file.media_type)
    .bind(file.width)
    .bind(file.height)
    .bind(file.duration_ms)
    .bind(file.frames)
    .bind(file.has_audio)
    .bind(file.file_size)
    .bind(file.storage_key)
    .bind(file.phash)
    .bind(file.pixel_hash)
    .bind(file.traits)
    .fetch_one(db)
    .await
}

/// Adds a file to an upload that is still to be downloaded from
/// `file_url`.
pub async fn create_pending(
    db: impl PgExecutor<'_>,
    slot: Slot<'_>,
    file_url: &str,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO staged_uploads (upload_id, uploader_id, position, file_name, source,
                                     status, file_url)
         VALUES ($1, $2, $3, $4, $5, 'pending', $6) RETURNING id",
    )
    .bind(slot.upload_id)
    .bind(slot.uploader_id)
    .bind(slot.position)
    .bind(slot.file_name)
    .bind(slot.source)
    .bind(file_url)
    .fetch_one(db)
    .await
}

/// Adds a file to an upload that couldn't be stored, and why.
pub async fn create_failed(
    db: impl PgExecutor<'_>,
    slot: Slot<'_>,
    error: &str,
    duplicate_of: Option<i64>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO staged_uploads (upload_id, uploader_id, position, file_name, source,
                                     status, error, duplicate_of)
         VALUES ($1, $2, $3, $4, $5, 'failed', $6, $7) RETURNING id",
    )
    .bind(slot.upload_id)
    .bind(slot.uploader_id)
    .bind(slot.position)
    .bind(slot.file_name)
    .bind(slot.source)
    .bind(error)
    .bind(duplicate_of)
    .fetch_one(db)
    .await
}

/// Records that a pending file is being downloaded now, so it isn't taken
/// for abandoned.
pub async fn started(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE staged_uploads SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Fills in a pending file once it's stored.
pub async fn stored(db: impl PgExecutor<'_>, id: i64, file: StoredFile<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE staged_uploads
         SET status = 'ready', sha256 = $2, md5 = $3, media_type = $4, width = $5, height = $6,
             duration_ms = $7, frames = $8, has_audio = $9, file_size = $10,
             storage_key = $11, phash = $12, pixel_hash = $13, traits = $14,
             updated_at = now()
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(file.sha256)
    .bind(file.md5)
    .bind(file.media_type)
    .bind(file.width)
    .bind(file.height)
    .bind(file.duration_ms)
    .bind(file.frames)
    .bind(file.has_audio)
    .bind(file.file_size)
    .bind(file.storage_key)
    .bind(file.phash)
    .bind(file.pixel_hash)
    .bind(file.traits)
    .execute(db)
    .await?;
    Ok(())
}

/// Marks a pending file failed.
pub async fn failed(
    db: impl PgExecutor<'_>,
    id: i64,
    error: &str,
    duplicate_of: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE staged_uploads
         SET status = 'failed', error = $2, duplicate_of = $3, updated_at = now()
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(error)
    .bind(duplicate_of)
    .execute(db)
    .await?;
    Ok(())
}

/// Gives up on upload `upload_id`'s pending files untouched for
/// `max_age` (whoever was downloading them stopped, as on a restart).
pub async fn fail_abandoned(
    db: impl PgExecutor<'_>,
    upload_id: i64,
    max_age: std::time::Duration,
    error: &str,
) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE staged_uploads SET status = 'failed', error = $3, updated_at = now()
         WHERE upload_id = $1 AND status = 'pending'
           AND updated_at < now() - make_interval(secs => $2)",
    )
    .bind(upload_id)
    .bind(max_age.as_secs_f64())
    .bind(error)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Staged>> {
    sqlx::query_as("SELECT * FROM staged_uploads WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
}

/// How many of user `uploader_id`'s files wait to be posted (or to be
/// downloaded).
pub async fn waiting(db: impl PgExecutor<'_>, uploader_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM staged_uploads
         WHERE uploader_id = $1 AND post_id IS NULL AND status <> 'failed'",
    )
    .bind(uploader_id)
    .fetch_one(db)
    .await
}

/// Upload `upload_id`'s files, in order.
pub async fn of_upload(db: impl PgExecutor<'_>, upload_id: i64) -> sqlx::Result<Vec<Staged>> {
    sqlx::query_as("SELECT * FROM staged_uploads WHERE upload_id = $1 ORDER BY position, id")
        .bind(upload_id)
        .fetch_all(db)
        .await
}

/// Which files a list of them shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter<'a> {
    /// One user's; everyone's when `None`.
    pub uploader_id: Option<i64>,
    pub status: Option<Status>,
    /// Posted, or not yet.
    pub posted: Option<bool>,
    /// `media_assets.media_type`.
    pub media_type: Option<&'a str>,
    /// The start of the source or of the link the file came from (`*`
    /// matches anything), regardless of case.
    pub source: Option<&'a str>,
}

/// Files matching `filter`, newest upload first, from `offset`.
pub async fn search(
    db: impl PgExecutor<'_>,
    filter: &Filter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Staged>> {
    let mut sql = sqlx::QueryBuilder::new("SELECT * FROM staged_uploads WHERE true");
    if let Some(uploader) = filter.uploader_id {
        sql.push(" AND uploader_id = ").push_bind(uploader);
    }
    if let Some(status) = filter.status {
        sql.push(" AND status = ").push_bind(status.as_str());
    }
    match filter.posted {
        Some(true) => sql.push(" AND post_id IS NOT NULL"),
        Some(false) => sql.push(" AND post_id IS NULL"),
        None => &mut sql,
    };
    if let Some(media_type) = filter.media_type {
        sql.push(" AND media_type = ")
            .push_bind(media_type.to_owned());
    }
    if let Some(source) = filter.source {
        let pattern = crate::tags::like_pattern(source);
        sql.push(" AND (source ILIKE ")
            .push_bind(pattern.clone())
            .push(" OR file_url ILIKE ")
            .push_bind(pattern)
            .push(")");
    }
    sql.push(" ORDER BY upload_id DESC, position, id OFFSET ")
        .push_bind(offset)
        .push(" LIMIT ")
        .push_bind(limit);
    sql.build_query_as().fetch_all(db).await
}

/// Records the post a staged upload became; false if it already became
/// one.
pub async fn used(db: impl PgExecutor<'_>, id: i64, post_id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE staged_uploads SET post_id = $2, updated_at = now()
         WHERE id = $1 AND post_id IS NULL",
    )
    .bind(id)
    .bind(post_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Removes staged uploads unused for longer than `max_age`, and uploads
/// left without any, and returns the storage keys no post or other staged
/// upload uses any more, for the caller to delete.
pub async fn expire(db: &PgPool, max_age: std::time::Duration) -> sqlx::Result<Vec<String>> {
    let mut tx = db.begin().await?;
    let keys: Vec<String> = sqlx::query_scalar(
        "WITH gone AS (
             DELETE FROM staged_uploads
             WHERE post_id IS NULL AND created_at < now() - make_interval(secs => $1)
             RETURNING id, storage_key
         )
         -- The statement still sees the rows it deletes, so they're left
         -- out by id.
         SELECT DISTINCT g.storage_key FROM gone g
         WHERE g.storage_key IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM media_assets a WHERE a.storage_key = g.storage_key)
           AND NOT EXISTS (SELECT 1 FROM staged_uploads s
                           WHERE s.storage_key = g.storage_key
                             AND s.id NOT IN (SELECT id FROM gone))",
    )
    .bind(max_age.as_secs_f64())
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM uploads u
         WHERE created_at < now() - make_interval(secs => $1)
           AND NOT EXISTS (SELECT 1 FROM staged_uploads s WHERE s.upload_id = u.id)",
    )
    .bind(max_age.as_secs_f64())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(key: &str) -> StoredFile<'_> {
        StoredFile {
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
            phash: None,
            pixel_hash: None,
            traits: &[],
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn stages_uses_and_expires(pool: PgPool) {
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let upload = create_upload(&pool, user, "", "").await.unwrap();
        let slot = |position| Slot {
            upload_id: upload,
            uploader_id: user,
            position,
            file_name: "a.png",
            source: "",
        };
        let used_one = create(&pool, slot(0), file("original/a.png"))
            .await
            .unwrap();
        let unused = create(&pool, slot(1), file("original/b.png"))
            .await
            .unwrap();
        let broken = create_failed(&pool, slot(2), "not an image", None)
            .await
            .unwrap();
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
        sqlx::query("UPDATE uploads SET created_at = now() - interval '2 days'")
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
        assert!(by_id(&pool, broken).await.unwrap().is_none());
        assert!(by_id(&pool, used_one).await.unwrap().is_some());
        // The upload stays while a file of it is a post.
        assert!(upload_by_id(&pool, upload).await.unwrap().is_some());
        sqlx::query("DELETE FROM staged_uploads")
            .execute(&pool)
            .await
            .unwrap();
        expire(&pool, std::time::Duration::from_secs(3600))
            .await
            .unwrap();
        assert!(upload_by_id(&pool, upload).await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn pending_files_are_stored_or_given_up(pool: PgPool) {
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let upload = create_upload(&pool, user, "https://example.com/work", "")
            .await
            .unwrap();
        let slot = |position| Slot {
            upload_id: upload,
            uploader_id: user,
            position,
            file_name: "https://example.com/1.png",
            source: "https://example.com/work",
        };
        let first = create_pending(&pool, slot(0), "https://example.com/1.png")
            .await
            .unwrap();
        let second = create_pending(&pool, slot(1), "https://example.com/2.png")
            .await
            .unwrap();
        stored(&pool, first, file("original/a.png")).await.unwrap();
        let hour = std::time::Duration::from_secs(3600);
        assert_eq!(
            fail_abandoned(&pool, upload, hour, "gone").await.unwrap(),
            0
        );
        sqlx::query("UPDATE staged_uploads SET updated_at = now() - interval '2 hours'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            fail_abandoned(&pool, upload, hour, "gone").await.unwrap(),
            1
        );
        let files = of_upload(&pool, upload).await.unwrap();
        assert_eq!(
            files
                .iter()
                .map(|f| (f.id, f.status, f.error.as_deref()))
                .collect::<Vec<_>>(),
            [
                (first, Status::Ready, None),
                (second, Status::Failed, Some("gone"))
            ]
        );
        assert_eq!(files[0].storage_key.as_deref(), Some("original/a.png"));
        // Neither can change once settled.
        failed(&pool, first, "late", None).await.unwrap();
        assert_eq!(
            by_id(&pool, first).await.unwrap().unwrap().status,
            Status::Ready
        );
        let mine = Filter {
            uploader_id: Some(user),
            ..Filter::default()
        };
        assert_eq!(search(&pool, &mine, 0, 10).await.unwrap().len(), 2);
        let found = |filter: Filter<'static>| {
            let pool = pool.clone();
            async move {
                search(&pool, &filter, 0, 10)
                    .await
                    .unwrap()
                    .iter()
                    .map(|f| f.id)
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(
            found(Filter {
                status: Some(Status::Failed),
                ..Filter::default()
            })
            .await,
            [second]
        );
        assert_eq!(
            found(Filter {
                media_type: Some("png"),
                posted: Some(false),
                ..Filter::default()
            })
            .await,
            [first]
        );
        assert_eq!(
            found(Filter {
                source: Some("HTTPS://example.com/2"),
                ..Filter::default()
            })
            .await,
            [second]
        );
        assert!(
            found(Filter {
                posted: Some(true),
                ..Filter::default()
            })
            .await
            .is_empty()
        );
        // Only the ready one waits to be posted.
        assert_eq!(waiting(&pool, user).await.unwrap(), 1);
    }
}
