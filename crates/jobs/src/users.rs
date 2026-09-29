//! Scheduled jobs about accounts: `users.promote` promotes members whose
//! record meets the site's rules, when automatic promotion is on, and
//! `users.prune_ips` forgets old addresses.

use std::time::Duration;

use moekura_core::jobs::{PromoteUsers, PruneIpHistory};
use moekura_db::{promotion, settings, user_ips};
use sqlx::PgPool;

use crate::{JobError, Registry};

/// How often members are checked.
pub const PROMOTION_EVERY: Duration = Duration::from_secs(60 * 60);

/// How often old addresses are forgotten.
pub const PRUNE_IPS_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct UserJobs {
    pub db: PgPool,
}

impl UserJobs {
    pub fn register(self, registry: &mut Registry) {
        let jobs = self.clone();
        registry
            .register(move |_: PromoteUsers| {
                let jobs = jobs.clone();
                async move { jobs.promote().await.map(|_| ()) }
            })
            .every::<PromoteUsers>(PROMOTION_EVERY);
        registry
            .register(move |_: PruneIpHistory| {
                let jobs = self.clone();
                async move { jobs.prune_ips().await.map(|_| ()) }
            })
            .every::<PruneIpHistory>(PRUNE_IPS_EVERY);
    }

    /// Forgets addresses unused for longer than the site keeps them;
    /// returns how many.
    pub async fn prune_ips(&self) -> Result<u64, JobError> {
        let days = settings::load(&self.db).await?.ip_history_days;
        let pruned = user_ips::prune(&self.db, days).await?;
        if pruned > 0 {
            tracing::info!(pruned, "forgot old addresses");
        }
        Ok(pruned)
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
    async fn old_addresses_are_forgotten(pool: PgPool) {
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles
             WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO user_ips (user_id, ip, last_seen_at)
             VALUES ($1, '203.0.113.1', now() - interval '400 days'),
                    ($1, '203.0.113.2', now() - interval '2 days')",
        )
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
        let jobs = UserJobs { db: pool.clone() };
        assert_eq!(jobs.prune_ips().await.unwrap(), 1, "kept for a year");
        settings::set(&pool, "ip_history_days", json!(0))
            .await
            .unwrap();
        assert_eq!(jobs.prune_ips().await.unwrap(), 1, "none kept");
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
