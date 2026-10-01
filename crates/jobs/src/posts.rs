//! The `posts.batch` job: moderating many posts at once (`post_batches`),
//! such as deleting every upload of a spam account or purging deleted
//! posts, in batches.

use moekura_core::jobs::PostBatch;
use moekura_db::post_batches::{self, Progress};
use moekura_db::search::{PageRef, Plan};
use moekura_storage::Storage;
use sqlx::PgPool;

use crate::media::purge_post;
use crate::tags::{search_error, search_plan};
use crate::{JobError, Registry};

/// Posts handled per transaction.
const BATCH: i64 = 100;

/// Posts in a row a purge may fail to remove before the job stops, to be
/// retried later from the first of them: storage is probably down.
const FAILURES_IN_A_ROW: u32 = 5;

#[derive(Clone)]
pub struct PostJobs {
    pub db: PgPool,
    pub storage: Storage,
}

/// The posts a purge goes through.
enum Targets {
    /// Those ticked, newest first.
    Ticked(Vec<i64>),
    Search(Box<Plan>),
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
            "purge" => self.purge(&batch).await?,
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
            total += progress;
            before = Some(last);
        }
        Ok(total)
    }

    /// Purges the deleted posts the batch names, each as a single purge
    /// would; posts restored or purged meanwhile are skipped, and those
    /// whose files can't be removed are counted as failed and left.
    async fn purge(&self, batch: &post_batches::PostBatch) -> Result<Progress, JobError> {
        let targets = match (&batch.post_ids, &batch.query) {
            (Some(ids), _) => Targets::Ticked(ids.clone()),
            (None, Some(query)) => Targets::Search(Box::new(search_plan(&self.db, query).await?)),
            (None, None) => return Err(JobError::permanent("a purge without posts or a search")),
        };
        let mut total = Progress::default();
        let mut failures = 0;
        let mut before = batch.resume_before;
        loop {
            let ids = match &targets {
                Targets::Ticked(ids) => ids
                    .iter()
                    .copied()
                    .filter(|&id| before.is_none_or(|b| id < b))
                    .take(BATCH as usize)
                    .collect(),
                Targets::Search(plan) => {
                    let page = before.map_or(PageRef::Number(1), PageRef::Before);
                    plan.ids(&self.db, page).await.map_err(search_error)?
                }
            };
            let Some(&last) = ids.last() else { break };
            for id in ids {
                let progress = match purge_post(&self.db, &self.storage, id).await {
                    Ok(purged) => {
                        failures = 0;
                        Progress {
                            done: i32::from(purged),
                            skipped: i32::from(!purged),
                            failed: 0,
                        }
                    }
                    Err(error) => {
                        failures += 1;
                        if failures >= FAILURES_IN_A_ROW {
                            return Err(error);
                        }
                        tracing::warn!(post_id = id, %error, "could not purge a post");
                        Progress {
                            failed: 1,
                            ..Progress::default()
                        }
                    }
                };
                post_batches::record_purge(&self.db, batch, id, progress).await?;
                total += progress;
            }
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

    fn jobs(pool: &PgPool) -> PostJobs {
        let dir = std::env::temp_dir().join(format!("moekura-jobs-posts-{}", std::process::id()));
        PostJobs {
            db: pool.clone(),
            storage: Storage::local(dir).unwrap(),
        }
    }

    async fn exists(pool: &PgPool, id: i64) -> bool {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM posts WHERE id = $1)")
            .bind(id)
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
        let jobs = jobs(&pool);
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
        jobs(&pool).run(id).await.unwrap();
        assert_eq!(status(&pool, older).await, "deleted");
        assert_eq!(status(&pool, newer).await, "active", "already handled");
        let batch = post_batches::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!((batch.done, batch.skipped), (2, 0));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn purges_the_deleted_posts_a_search_finds(pool: PgPool) {
        let spammer = user(&pool, "spammer").await;
        let other = user(&pool, "other").await;
        let admin = user(&pool, "admin").await;
        let mut deleted = Vec::new();
        for _ in 0..3 {
            deleted.push(post(&pool, spammer, "deleted", &[]).await);
        }
        let active = post(&pool, spammer, "active", &[]).await;
        let theirs = post(&pool, other, "deleted", &[]).await;
        let id = post_batches::create_purge(
            &pool,
            Some(admin),
            Some("user:spammer status:deleted"),
            None,
            3,
        )
        .await
        .unwrap();
        let progress = jobs(&pool).run(id).await.unwrap();
        assert_eq!(progress.done, 3);
        for id in &deleted {
            assert!(!exists(&pool, *id).await);
        }
        assert!(exists(&pool, active).await && exists(&pool, theirs).await);
        let filter = Filter {
            action: Some(ActionKind::PostPurge),
            actor_id: Some(admin),
            ..Filter::default()
        };
        assert_eq!(
            mod_actions::list(&pool, &filter, 10).await.unwrap().len(),
            3
        );
        let batch = post_batches::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!((batch.status.as_str(), batch.done), ("done", 3));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn purges_ticked_posts_still_deleted(pool: PgPool) {
        let uploader = user(&pool, "uploader").await;
        let first = post(&pool, uploader, "deleted", &[]).await;
        let restored = post(&pool, uploader, "deleted", &[]).await;
        let second = post(&pool, uploader, "deleted", &[]).await;
        let left = post(&pool, uploader, "deleted", &[]).await;
        let id = post_batches::create_purge(&pool, None, None, Some(&[first, restored, second]), 3)
            .await
            .unwrap();
        // Restored after the purge was asked for.
        sqlx::query("UPDATE posts SET status = 'active' WHERE id = $1")
            .bind(restored)
            .execute(&pool)
            .await
            .unwrap();
        let progress = jobs(&pool).run(id).await.unwrap();
        assert_eq!(
            progress,
            Progress {
                done: 2,
                skipped: 1,
                failed: 0
            }
        );
        assert!(!exists(&pool, first).await && !exists(&pool, second).await);
        assert!(exists(&pool, restored).await, "restored meanwhile");
        assert!(exists(&pool, left).await, "not ticked");
        // A retry has nothing left to do.
        sqlx::query("UPDATE post_batches SET status = 'running' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(jobs(&pool).run(id).await.unwrap(), Progress::default());
        let batch = post_batches::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!((batch.done, batch.skipped), (2, 1));
    }
}
