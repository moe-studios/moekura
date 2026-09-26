//! The `users.promote` job: promoting members whose record meets the
//! site's rules, when automatic promotion is on.

use std::time::Duration;

use moekura_core::jobs::PromoteUsers;
use moekura_db::{promotion, settings};
use sqlx::PgPool;

use crate::{JobError, Registry};

/// How often members are checked.
pub const PROMOTION_EVERY: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct UserJobs {
    pub db: PgPool,
}

impl UserJobs {
    pub fn register(self, registry: &mut Registry) {
        registry
            .register(move |_: PromoteUsers| {
                let jobs = self.clone();
                async move { jobs.promote().await.map(|_| ()) }
            })
            .every::<PromoteUsers>(PROMOTION_EVERY);
    }

    /// Promotes every member who qualifies; returns how many.
    pub async fn promote(&self) -> Result<usize, JobError> {
        let site = settings::load(&self.db).await?;
        if !site.auto_promotion {
            return Ok(0);
        }
        let mut promoted = 0;
        for candidate in promotion::candidates(&self.db, &site.promotion_rules).await? {
            if promotion::promote(&self.db, &candidate).await? {
                tracing::info!(user = candidate.name, "promoted to contributor");
                promoted += 1;
            }
        }
        Ok(promoted)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn promotes_only_when_turned_on(pool: PgPool) {
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id, created_at)
             SELECT 'alice', id, now() - interval '60 days' FROM roles WHERE system_key = 'member'
             RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO posts (rating, uploader_id) SELECT 'g', $1 FROM generate_series(1, 50)",
        )
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
        let jobs = UserJobs { db: pool.clone() };
        assert_eq!(jobs.promote().await.unwrap(), 0);
        settings::set(&pool, "auto_promotion", json!(true))
            .await
            .unwrap();
        assert_eq!(jobs.promote().await.unwrap(), 1);
        assert_eq!(jobs.promote().await.unwrap(), 0);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn scheduled_jobs_are_enqueued_once(pool: PgPool) {
        assert!(
            moekura_db::jobs::enqueue_unique(&pool, &PromoteUsers::default())
                .await
                .unwrap()
        );
        assert!(
            !moekura_db::jobs::enqueue_unique(&pool, &PromoteUsers::default())
                .await
                .unwrap()
        );
    }
}
