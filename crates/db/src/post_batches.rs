//! Moderation of many posts at once (`post_batches`): deleting every
//! upload of a user, by a job in batches.

use moekura_core::moderation::ActionKind;
use moekura_core::posts::{PostLock, PostStatus};
use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

use crate::mod_actions::{self, NewAction};
use crate::{flags, posts};

/// The statuses a post can be deleted from.
pub const DELETABLE: [PostStatus; 3] =
    [PostStatus::Active, PostStatus::Flagged, PostStatus::Pending];

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PostBatch {
    pub id: i64,
    /// `delete`.
    pub kind: String,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    /// Whose uploads, for deletions.
    pub user_id: Option<i64>,
    pub user_name: Option<String>,
    pub reason: String,
    pub override_locks: bool,
    /// `queued`, `running`, `done` or `failed`.
    pub status: String,
    pub total: i32,
    pub done: i32,
    pub skipped: i32,
    pub failed: i32,
    pub resume_before: Option<i64>,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
}

impl PostBatch {
    /// Whether it's still to finish.
    pub fn is_open(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }
}

macro_rules! select_batches {
    ($rest:literal) => {
        concat!(
            "SELECT b.id, b.kind, b.creator_id, c.name::text AS creator_name, b.user_id,
                    u.name::text AS user_name, b.reason, b.override_locks, b.status, b.total,
                    b.done, b.skipped, b.failed, b.resume_before, b.error, b.created_at,
                    b.finished_at
             FROM post_batches b
             LEFT JOIN users c ON c.id = b.creator_id
             LEFT JOIN users u ON u.id = b.user_id ",
            $rest
        )
    };
}

/// Records a deletion of user `user_id`'s `total` deletable uploads;
/// returns its id.
pub async fn create_deletion(
    db: impl PgExecutor<'_>,
    creator_id: Option<i64>,
    user_id: i64,
    reason: &str,
    override_locks: bool,
    total: i64,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO post_batches (kind, creator_id, user_id, reason, override_locks, total)
         VALUES ('delete', $1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(creator_id)
    .bind(user_id)
    .bind(reason)
    .bind(override_locks)
    .bind(i32::try_from(total).unwrap_or(i32::MAX))
    .fetch_one(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<PostBatch>> {
    sqlx::query_as(select_batches!("WHERE b.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The latest batches of kind `kind`, newest first; only those about
/// user `user_id` if given.
pub async fn recent(
    db: impl PgExecutor<'_>,
    kind: &str,
    user_id: Option<i64>,
    limit: i64,
) -> sqlx::Result<Vec<PostBatch>> {
    sqlx::query_as(select_batches!(
        "WHERE b.kind = $1 AND ($2::bigint IS NULL OR b.user_id = $2)
         ORDER BY b.id DESC LIMIT $3"
    ))
    .bind(kind)
    .bind(user_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Whether a deletion of user `user_id`'s uploads is still to finish.
pub async fn deletion_open(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM post_batches
                        WHERE kind = 'delete' AND user_id = $1 AND status IN ('queued', 'running'))",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

/// Marks a batch running. Its counts are kept: a retried job carries on
/// from `resume_before`.
pub async fn start(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE post_batches SET status = 'running', error = NULL WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Marks a batch done, or failed with `error`.
pub async fn finish(db: impl PgExecutor<'_>, id: i64, error: Option<&str>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE post_batches SET status = CASE WHEN $2::text IS NULL THEN 'done' ELSE 'failed' END,
                                 error = $2, finished_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

/// What a batch of posts came to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    pub done: i32,
    pub skipped: i32,
    pub failed: i32,
}

/// Adds `progress` to batch `id`'s counts; posts from `resume_before` up
/// are finished with.
pub async fn advance(
    db: impl PgExecutor<'_>,
    id: i64,
    progress: Progress,
    resume_before: i64,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE post_batches SET done = done + $2, skipped = skipped + $3, failed = failed + $4,
                                 resume_before = $5
         WHERE id = $1",
    )
    .bind(id)
    .bind(progress.done)
    .bind(progress.skipped)
    .bind(progress.failed)
    .bind(resume_before)
    .execute(db)
    .await?;
    Ok(())
}

/// How many of user `user_id`'s uploads could be deleted.
pub async fn count_deletable(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM posts WHERE uploader_id = $1 AND status = ANY($2)")
        .bind(user_id)
        .bind(DELETABLE.map(PostStatus::as_str))
        .fetch_one(db)
        .await
}

/// Up to `limit` of user `user_id`'s deletable uploads below post
/// `before`, newest first.
pub async fn deletable_uploads(
    db: impl PgExecutor<'_>,
    user_id: i64,
    before: Option<i64>,
    limit: i64,
) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM posts
         WHERE uploader_id = $1 AND status = ANY($2) AND ($3::bigint IS NULL OR id < $3)
         ORDER BY id DESC LIMIT $4",
    )
    .bind(user_id)
    .bind(DELETABLE.map(PostStatus::as_str))
    .bind(before)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Deletes posts `ids` (newest first) for deletion `batch`, as a single
/// deletion would: as its creator, with its reason in the log, settling
/// open flags as upheld. Posts dealt with meanwhile, or whose status is
/// locked when the creator couldn't change that, are skipped. The
/// batch's progress is recorded in the same transaction.
pub async fn delete_posts(db: &PgPool, batch: &PostBatch, ids: &[i64]) -> sqlx::Result<Progress> {
    let Some(&last) = ids.last() else {
        return Ok(Progress::default());
    };
    let mut progress = Progress::default();
    let mut tx = db.begin().await?;
    for &id in ids {
        let deletable = posts::lock(&mut *tx, id).await?.is_some_and(|post| {
            DELETABLE.contains(&post.status)
                && (batch.override_locks || !post.is_locked(PostLock::Status))
        });
        if !deletable {
            progress.skipped += 1;
            continue;
        }
        posts::set_status(&mut *tx, id, &DELETABLE, PostStatus::Deleted).await?;
        flags::resolve(&mut *tx, id, true, batch.creator_id).await?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(batch.creator_id, ActionKind::PostDelete)
                .post(id)
                .reason(&batch.reason)
                .details(serde_json::json!({ "batch": batch.id })),
        )
        .await?;
        progress.done += 1;
    }
    advance(&mut *tx, batch.id, progress, last).await?;
    tx.commit().await?;
    Ok(progress)
}
