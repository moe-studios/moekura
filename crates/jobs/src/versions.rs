//! The `post_versions.undo_user` job: undoing a user's post edits (for
//! vandalism), started by staff from the sitewide post history.

use moekura_core::jobs::UndoUserEdits;
use moekura_db::post_versions;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::{JobError, Registry};

#[derive(Clone)]
pub struct VersionJobs {
    pub db: PgPool,
}

impl VersionJobs {
    pub fn register(self, registry: &mut Registry) {
        registry.register(move |job: UndoUserEdits| {
            let jobs = self.clone();
            async move { jobs.undo(&job).await.map(|_| ()) }
        });
    }

    /// Undoes the edits `job` names; returns how many posts changed.
    /// Safe to repeat: edits already undone leave nothing to change.
    pub async fn undo(&self, job: &UndoUserEdits) -> Result<u64, JobError> {
        let time = |t: Option<i64>| -> Result<Option<OffsetDateTime>, JobError> {
            t.map(|t| {
                OffsetDateTime::from_unix_timestamp(t)
                    .map_err(|e| JobError::Permanent(format!("bad time {t}: {e}")))
            })
            .transpose()
        };
        let changed = post_versions::undo_user(
            &self.db,
            job.user_id,
            time(job.since)?,
            time(job.until)?,
            job.actor_id,
        )
        .await?;
        tracing::info!(
            user_id = job.user_id,
            posts = changed,
            "undid a user's post edits"
        );
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn undoing_nothing_changes_nothing(pool: PgPool) {
        let jobs = VersionJobs { db: pool };
        let job = UndoUserEdits {
            user_id: 1,
            since: Some(0),
            until: Some(OffsetDateTime::now_utc().unix_timestamp()),
            actor_id: None,
        };
        assert_eq!(jobs.undo(&job).await.unwrap(), 0);
        let bad = UndoUserEdits {
            since: Some(i64::MAX),
            ..job
        };
        assert!(matches!(jobs.undo(&bad).await, Err(JobError::Permanent(_))));
    }
}
