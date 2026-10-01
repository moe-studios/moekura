//! The `posts.batch` job: moderating many posts at once (`post_batches`),
//! such as deleting every upload of a spam account, in batches.

use moekura_core::jobs::PostBatch;
use moekura_db::post_batches::{self, Progress};
use sqlx::PgPool;

use crate::{JobError, Registry};

/// Posts handled per transaction.
const BATCH: i64 = 100;

#[derive(Clone)]
pub struct PostJobs {
    pub db: PgPool,
}

impl PostJobs {
    pub fn register(self, registry: &mut Registry) {
        registry.register(move |job: PostBatch| {
            let jobs = self.clone();
            async move {
                let result = jobs.run(job.id).await;
                if let Err(JobError::Permanent(error)) = &result {
                    post_batches::finish(&jobs.db, job.id, Some(error)).await?;
                }
                result.map(|_| ())
            }
        });
    }

    /// Works through batch `id` from where it stopped; returns what this
    /// run came to. Safe to repeat.
    pub async fn run(&self, id: i64) -> Result<Progress, JobError> {
        let Some(batch) = post_batches::by_id(&self.db, id).await? else {
            return Ok(Progress::default());
        };
        if !batch.is_open() {
            return Ok(Progress::default());
        }
        post_batches::start(&self.db, id).await?;
        let progress = match batch.kind.as_str() {
            "delete" => self.delete_uploads(&batch).await?,
            kind => return Err(JobError::permanent(format!("unknown batch kind `{kind}`"))),
        };
        post_batches::finish(&self.db, id, None).await?;
        tracing::info!(
            id,
            kind = batch.kind,
            done = progress.done,
            skipped = progress.skipped,
            failed = progress.failed,
            "post batch done"
        );
        Ok(progress)
    }

    /// Deletes the uploads of the batch's user that can be deleted.
    async fn delete_uploads(&self, batch: &post_batches::PostBatch) -> Result<Progress, JobError> {
        // The account is gone, and its uploads have no uploader to find
        // them by.
        let Some(user_id) = batch.user_id else {
            return Ok(Progress::default());
        };
        let mut total = Progress::default();
        let mut before = batch.resume_before;
        loop {
            let ids = post_batches::deletable_uploads(&self.db, user_id, before, BATCH).await?;
            let Some(&last) = ids.last() else { break };
            let progress = post_batches::delete_posts(&self.db, batch, &ids).await?;
            total.done += progress.done;
            total.skipped += progress.skipped;
            before = Some(last);
        }
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use moekura_core::moderation::ActionKind;
    use moekura_db::mod_actions::{self, Filter};

    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member'
             RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn post(pool: &PgPool, uploader: i64, status: &str, locks: &[&str]) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO posts (rating, status, uploader_id, locks) VALUES ('g', $1, $2, $3)
             RETURNING id",
        )
        .bind(status)
        .bind(uploader)
        .bind(locks)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn status(pool: &PgPool, id: i64) -> String {
        sqlx::query_scalar("SELECT status FROM posts WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn deletes_a_users_uploads_in_batches(pool: PgPool) {
        let spammer = user(&pool, "spammer").await;
        let other = user(&pool, "other").await;
        let moderator = user(&pool, "mod").await;
        let mut active = Vec::new();
        for _ in 0..(BATCH + 5) {
            active.push(post(&pool, spammer, "active", &[]).await);
        }
        let flagged = post(&pool, spammer, "flagged", &[]).await;
        let pending = post(&pool, spammer, "pending", &[]).await;
        let deleted = post(&pool, spammer, "deleted", &[]).await;
        let locked = post(&pool, spammer, "active", &["status"]).await;
        let theirs = post(&pool, other, "active", &[]).await;
        sqlx::query("INSERT INTO post_flags (post_id, creator_id, reason) VALUES ($1, $2, 'spam')")
            .bind(flagged)
            .bind(other)
            .execute(&pool)
            .await
            .unwrap();

        let total = post_batches::count_deletable(&pool, spammer).await.unwrap();
        assert_eq!(total, BATCH + 5 + 3);
        let id =
            post_batches::create_deletion(&pool, Some(moderator), spammer, "spam", false, total)
                .await
                .unwrap();
        let jobs = PostJobs { db: pool.clone() };
        let progress = jobs.run(id).await.unwrap();
        assert_eq!(
            progress,
            Progress {
                done: BATCH as i32 + 7,
                skipped: 1,
                failed: 0
            }
        );
        for id in active.iter().chain([&flagged, &pending, &deleted]) {
            assert_eq!(status(&pool, *id).await, "deleted");
        }
        assert_eq!(
            status(&pool, locked).await,
            "active",
            "the status is locked"
        );
        assert_eq!(status(&pool, theirs).await, "active");
        let flag: String = sqlx::query_scalar("SELECT status FROM post_flags")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(flag, "upheld");

        let batch = post_batches::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (batch.status.as_str(), batch.done, batch.skipped),
            ("done", BATCH as i32 + 7, 1)
        );
        // Each deletion is logged as the moderator's, with the reason.
        let filter = Filter {
            action: Some(ActionKind::PostDelete),
            actor_id: Some(moderator),
            ..Filter::default()
        };
        let logged = mod_actions::list(&pool, &filter, 1000).await.unwrap();
        assert_eq!(logged.len(), BATCH as usize + 7);
        assert!(logged.iter().all(|e| e.reason == "spam"));
        // Running it again changes nothing.
        assert_eq!(jobs.run(id).await.unwrap(), Progress::default());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn a_retry_carries_on_where_it_stopped(pool: PgPool) {
        let spammer = user(&pool, "spammer").await;
        let older = post(&pool, spammer, "active", &[]).await;
        let newer = post(&pool, spammer, "active", &["status"]).await;
        let id = post_batches::create_deletion(&pool, None, spammer, "spam", true, 2)
            .await
            .unwrap();
        // As if an earlier attempt deleted `newer`, then stopped.
        let batch = post_batches::by_id(&pool, id).await.unwrap().unwrap();
        let progress = post_batches::delete_posts(&pool, &batch, &[newer])
            .await
            .unwrap();
        assert_eq!(progress.done, 1, "the creator could change locked statuses");
        sqlx::query("UPDATE posts SET status = 'active' WHERE id = $1")
            .bind(newer)
            .execute(&pool)
            .await
            .unwrap();
        PostJobs { db: pool.clone() }.run(id).await.unwrap();
        assert_eq!(status(&pool, older).await, "deleted");
        assert_eq!(status(&pool, newer).await, "active", "already handled");
        let batch = post_batches::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!((batch.done, batch.skipped), (2, 0));
    }
}
