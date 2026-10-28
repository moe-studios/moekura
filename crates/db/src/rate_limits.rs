//! Rate-limit counters in the database (`rate_limits`), for sites without
//! Valkey: the same GCRA as the web servers' own counters and Valkey's,
//! shared by every server.

use std::time::Duration;

use sqlx::PgExecutor;

/// Counts one more against `key`, allowing `burst` at once and then one
/// every `period`: `None` when allowed, or how long to wait.
pub async fn gcra(
    db: impl PgExecutor<'_> + Copy,
    key: &str,
    burst: u32,
    period: Duration,
) -> sqlx::Result<Option<Duration>> {
    let period = period.as_secs_f64();
    let allowed: Option<i32> = sqlx::query_scalar(
        "INSERT INTO rate_limits AS r (key, tat)
         VALUES ($1, now() + make_interval(secs => $2))
         ON CONFLICT (key) DO UPDATE
             SET tat = greatest(r.tat, now()) + make_interval(secs => $2)
             WHERE greatest(r.tat, now()) + make_interval(secs => $2 - $3 * $2) <= now()
         RETURNING 1",
    )
    .bind(key)
    .bind(period)
    .bind(f64::from(burst))
    .fetch_optional(db)
    .await?;
    if allowed.is_some() {
        return Ok(None);
    }
    let wait: f64 = sqlx::query_scalar(
        "SELECT greatest(extract(epoch FROM greatest(tat, now())
                                 + make_interval(secs => $2 - $3 * $2) - now())::float8, 0)
         FROM rate_limits WHERE key = $1",
    )
    .bind(key)
    .bind(period)
    .bind(f64::from(burst))
    .fetch_optional(db)
    .await?
    .unwrap_or(0.0);
    Ok(Some(Duration::from_secs_f64(wait.max(0.001))))
}

/// Forgets counters back at their full allowance; returns how many.
pub async fn prune(db: impl PgExecutor<'_>) -> sqlx::Result<u64> {
    let done = sqlx::query("DELETE FROM rate_limits WHERE tat < now()")
        .execute(db)
        .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn a_burst_then_one_a_period(pool: PgPool) {
        let period = Duration::from_secs(60);
        for _ in 0..3 {
            assert_eq!(gcra(&pool, "k", 3, period).await.unwrap(), None);
        }
        let wait = gcra(&pool, "k", 3, period).await.unwrap().unwrap();
        assert!(wait > Duration::from_secs(50) && wait <= period, "{wait:?}");
        assert_eq!(gcra(&pool, "other", 3, period).await.unwrap(), None);
        // A minute on, one more.
        sqlx::query("UPDATE rate_limits SET tat = tat - interval '60 seconds' WHERE key = 'k'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(gcra(&pool, "k", 3, period).await.unwrap(), None);
        assert!(gcra(&pool, "k", 3, period).await.unwrap().is_some());
        sqlx::query("UPDATE rate_limits SET tat = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(prune(&pool).await.unwrap(), 2);
    }
}
