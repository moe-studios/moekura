//! `stats.refresh`: counts site statistics for the stats and reports
//! pages. Each run recounts yesterday and today; the first run for a
//! metric counts the last year.

use std::time::Duration;

use moekura_core::jobs::RefreshStats;
use moekura_db::reports::{self, Metric};
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::{JobError, Registry};

/// How often the statistics are counted.
pub const STATS_EVERY: Duration = Duration::from_secs(60 * 60);
/// How far back the first count goes.
pub const BACKFILL_DAYS: i64 = 365;

#[derive(Clone)]
pub struct StatsJobs {
    pub db: PgPool,
}

impl StatsJobs {
    pub fn register(self, registry: &mut Registry) {
        registry
            .register(move |_: RefreshStats| {
                let jobs = self.clone();
                async move { jobs.refresh().await }
            })
            .every::<RefreshStats>(STATS_EVERY);
    }

    pub async fn refresh(&self) -> Result<(), JobError> {
        let today = OffsetDateTime::now_utc().date();
        for metric in Metric::ALL {
            let days = if reports::first_day(&self.db, metric).await?.is_some() {
                1
            } else {
                BACKFILL_DAYS
            };
            let from = today - time::Duration::days(days);
            reports::refresh(&self.db, metric, from, today).await?;
        }
        reports::refresh_totals(&self.db).await?;
        tracing::debug!("site statistics counted");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn counts_the_past_year_first(pool: PgPool) {
        sqlx::query(
            "INSERT INTO posts (rating, created_at) VALUES
             ('g', now()), ('g', now() - interval '100 days'), ('g', now() - interval '400 days')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let jobs = StatsJobs { db: pool.clone() };
        jobs.refresh().await.unwrap();
        let counted: i64 = sqlx::query_scalar(
            "SELECT sum(count)::bigint FROM daily_stats WHERE metric = 'uploads' AND user_id = 0",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counted, 2);
        assert_eq!(reports::totals(&pool).await.unwrap()[0].1, 3);
        // Later runs only recount the last days.
        sqlx::query("DELETE FROM posts WHERE created_at < now() - interval '50 days'")
            .execute(&pool)
            .await
            .unwrap();
        jobs.refresh().await.unwrap();
        let counted: i64 = sqlx::query_scalar(
            "SELECT sum(count)::bigint FROM daily_stats WHERE metric = 'uploads' AND user_id = 0",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(counted, 2);
    }
}
