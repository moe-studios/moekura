//! Queries on `jobs`: the queue itself. The worker loop lives in the
//! `moekura-jobs` crate.

use std::time::Duration;

use moekura_core::jobs::Job;
use serde_json::Value;
use sqlx::{PgConnection, PgExecutor};

/// Workers `LISTEN` here; enqueueing notifies it.
pub const CHANNEL: &str = "moekura_jobs";

/// Adds a job, delivered to workers when the surrounding transaction (if
/// any) commits. Returns the job id.
pub async fn enqueue<J: Job>(conn: &mut PgConnection, job: &J) -> sqlx::Result<i64> {
    enqueue_at(conn, job, Duration::ZERO).await
}

/// Enqueues `job` unless a job of its kind is already waiting or running;
/// returns whether it did. For scheduled jobs, where several nodes may try
/// at once and one run is enough.
pub async fn enqueue_unique<J: Job>(db: &sqlx::PgPool, job: &J) -> sqlx::Result<bool> {
    let payload = serde_json::to_value(job).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let mut tx = db.begin().await?;
    // Serialises nodes scheduling the same kind.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(J::KIND)
        .execute(&mut *tx)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO jobs (kind, payload, max_attempts)
         SELECT $1, $2, $3
         WHERE NOT EXISTS (SELECT 1 FROM jobs WHERE kind = $1 AND status IN ('queued', 'running'))",
    )
    .bind(J::KIND)
    .bind(payload)
    .bind(J::MAX_ATTEMPTS)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        sqlx::query("SELECT pg_notify($1, '')")
            .bind(CHANNEL)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(inserted)
}

/// Like [`enqueue`], but the job becomes runnable after `delay`.
pub async fn enqueue_at<J: Job>(
    conn: &mut PgConnection,
    job: &J,
    delay: Duration,
) -> sqlx::Result<i64> {
    let payload = serde_json::to_value(job).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let id = sqlx::query_scalar(
        "INSERT INTO jobs (kind, payload, max_attempts, run_at)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4))
         RETURNING id",
    )
    .bind(J::KIND)
    .bind(payload)
    .bind(J::MAX_ATTEMPTS)
    .bind(delay.as_secs_f64())
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query("SELECT pg_notify($1, '')")
        .bind(CHANNEL)
        .execute(&mut *conn)
        .await?;
    Ok(id)
}

/// A job a worker has claimed.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedJob {
    pub id: i64,
    pub kind: String,
    pub payload: Value,
    /// Including this one.
    pub attempts: i32,
    pub max_attempts: i32,
}

/// Takes the next runnable job, locking it for `lock` (extend with
/// [`heartbeat`] for longer work). Concurrent workers never get the same job.
pub async fn claim(
    db: impl PgExecutor<'_>,
    worker: &str,
    lock: Duration,
) -> sqlx::Result<Option<ClaimedJob>> {
    sqlx::query_as(
        "UPDATE jobs
         SET status = 'running', attempts = attempts + 1, locked_by = $1,
             locked_until = now() + make_interval(secs => $2)
         WHERE id = (
             SELECT id FROM jobs
             WHERE status = 'queued' AND run_at <= now()
             ORDER BY run_at, id
             FOR UPDATE SKIP LOCKED
             LIMIT 1
         )
         RETURNING id, kind, payload, attempts, max_attempts",
    )
    .bind(worker)
    .bind(lock.as_secs_f64())
    .fetch_optional(db)
    .await
}

/// Extends a running job's lock. Returns false if the job is no longer ours
/// (it was reclaimed after our lock expired).
pub async fn heartbeat(
    db: impl PgExecutor<'_>,
    id: i64,
    worker: &str,
    lock: Duration,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE jobs SET locked_until = now() + make_interval(secs => $3)
         WHERE id = $1 AND locked_by = $2 AND status = 'running'",
    )
    .bind(id)
    .bind(worker)
    .bind(lock.as_secs_f64())
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Removes a finished job.
pub async fn complete(db: impl PgExecutor<'_>, id: i64, worker: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM jobs WHERE id = $1 AND locked_by = $2")
        .bind(id)
        .bind(worker)
        .execute(db)
        .await?;
    Ok(())
}

/// Records a failure: the job is retried after `retry_in`, or marked dead
/// when it is out of attempts or `permanent`.
pub async fn fail(
    db: impl PgExecutor<'_>,
    id: i64,
    worker: &str,
    error: &str,
    retry_in: Duration,
    permanent: bool,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE jobs
         SET status = CASE WHEN $5 OR attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
             run_at = now() + make_interval(secs => $4),
             locked_by = NULL, locked_until = NULL, last_error = $3
         WHERE id = $1 AND locked_by = $2",
    )
    .bind(id)
    .bind(worker)
    .bind(truncate(error, 4000))
    .bind(retry_in.as_secs_f64())
    .bind(permanent)
    .execute(db)
    .await?;
    Ok(())
}

/// Requeues running jobs whose lock expired (their worker crashed or was
/// killed), or marks them dead if that was their last attempt. Returns how
/// many were recovered.
pub async fn recover_abandoned(db: impl PgExecutor<'_>) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE jobs
         SET status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
             locked_by = NULL, locked_until = NULL,
             last_error = coalesce(last_error, 'worker stopped responding')
         WHERE status = 'running' AND locked_until < now()",
    )
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// Delay before retry number `attempts` (1-based): 10s, 20s, 40s … capped
/// at an hour.
pub fn backoff(attempts: i32) -> Duration {
    let exponent = attempts.clamp(1, 20) as u32 - 1;
    Duration::from_secs((10u64 << exponent).min(3600))
}

fn truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct JobCounts {
    pub queued: i64,
    pub running: i64,
    pub dead: i64,
}

pub async fn counts(db: impl PgExecutor<'_>) -> sqlx::Result<JobCounts> {
    sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'queued') AS queued,
                count(*) FILTER (WHERE status = 'running') AS running,
                count(*) FILTER (WHERE status = 'dead') AS dead
         FROM jobs",
    )
    .fetch_one(db)
    .await
}

/// Job counts per kind and status.
pub async fn counts_by_kind(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<(String, String, i64)>> {
    sqlx::query_as(
        "SELECT kind, status, count(*) FROM jobs GROUP BY kind, status ORDER BY kind, status",
    )
    .fetch_all(db)
    .await
}

/// A job that ran out of attempts.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DeadJob {
    pub id: i64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub created_at: time::OffsetDateTime,
}

/// Dead jobs, newest first.
pub async fn dead(db: impl PgExecutor<'_>, limit: i64) -> sqlx::Result<Vec<DeadJob>> {
    sqlx::query_as(
        "SELECT id, kind, payload, attempts, last_error, created_at FROM jobs
         WHERE status = 'dead' ORDER BY id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Gives a dead job a fresh set of attempts; false if it isn't dead.
pub async fn retry(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE jobs SET status = 'queued', attempts = 0, run_at = now(), last_error = NULL
         WHERE id = $1 AND status = 'dead'",
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Deletes a dead job; false if it isn't dead.
pub async fn discard(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM jobs WHERE id = $1 AND status = 'dead'")
        .bind(id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use sqlx::PgPool;

    use super::*;

    #[derive(Serialize, Deserialize)]
    struct Ping {
        n: i32,
    }

    impl Job for Ping {
        const KIND: &'static str = "test.ping";
        const MAX_ATTEMPTS: i32 = 2;
    }

    const LOCK: Duration = Duration::from_secs(60);

    async fn push(pool: &PgPool, n: i32) -> i64 {
        let mut conn = pool.acquire().await.unwrap();
        enqueue(&mut conn, &Ping { n }).await.unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn claims_in_order_and_completes(pool: PgPool) {
        let first = push(&pool, 1).await;
        let second = push(&pool, 2).await;

        let job = claim(&pool, "w1", LOCK).await.unwrap().unwrap();
        assert_eq!(
            (job.id, job.kind.as_str(), job.attempts),
            (first, "test.ping", 1)
        );
        assert_eq!(job.payload["n"], 1);
        // A second worker gets the next job, not the claimed one.
        let other = claim(&pool, "w2", LOCK).await.unwrap().unwrap();
        assert_eq!(other.id, second);
        assert!(claim(&pool, "w3", LOCK).await.unwrap().is_none());

        complete(&pool, first, "w1").await.unwrap();
        assert_eq!(
            counts(&pool).await.unwrap(),
            JobCounts {
                queued: 0,
                running: 1,
                dead: 0
            }
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn concurrent_claims_never_share_a_job(pool: PgPool) {
        for n in 0..20 {
            push(&pool, n).await;
        }
        let claimers = (0..8).map(|w| {
            let pool = pool.clone();
            tokio::spawn(async move {
                let mut ids = Vec::new();
                while let Some(job) = claim(&pool, &format!("w{w}"), LOCK).await.unwrap() {
                    ids.push(job.id);
                }
                ids
            })
        });
        let mut all: Vec<i64> = Vec::new();
        for handle in claimers {
            all.extend(handle.await.unwrap());
        }
        all.sort_unstable();
        let before = all.len();
        all.dedup();
        assert_eq!((before, all.len()), (20, 20));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn enqueue_follows_the_transaction(pool: PgPool) {
        let mut tx = pool.begin().await.unwrap();
        enqueue(&mut tx, &Ping { n: 1 }).await.unwrap();
        tx.rollback().await.unwrap();
        assert!(claim(&pool, "w1", LOCK).await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn failures_retry_later_then_die(pool: PgPool) {
        let id = push(&pool, 1).await;

        let job = claim(&pool, "w1", LOCK).await.unwrap().unwrap();
        fail(
            &pool,
            job.id,
            "w1",
            "boom",
            Duration::from_secs(3600),
            false,
        )
        .await
        .unwrap();
        // Not runnable until the backoff passes.
        assert!(claim(&pool, "w1", LOCK).await.unwrap().is_none());

        sqlx::query("UPDATE jobs SET run_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        let job = claim(&pool, "w1", LOCK).await.unwrap().unwrap();
        assert_eq!(job.attempts, 2);
        fail(&pool, job.id, "w1", "boom again", Duration::ZERO, false)
            .await
            .unwrap();

        // MAX_ATTEMPTS = 2, so it is dead now and keeps its error.
        assert!(claim(&pool, "w1", LOCK).await.unwrap().is_none());
        let (status, error): (String, String) =
            sqlx::query_as("SELECT status, last_error FROM jobs WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((status.as_str(), error.as_str()), ("dead", "boom again"));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn permanent_failures_die_immediately(pool: PgPool) {
        push(&pool, 1).await;
        let job = claim(&pool, "w1", LOCK).await.unwrap().unwrap();
        fail(&pool, job.id, "w1", "unsupported", Duration::ZERO, true)
            .await
            .unwrap();
        assert_eq!(counts(&pool).await.unwrap().dead, 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn abandoned_jobs_are_recovered(pool: PgPool) {
        push(&pool, 1).await;
        let job = claim(&pool, "crashed", LOCK).await.unwrap().unwrap();
        assert_eq!(recover_abandoned(&pool).await.unwrap(), 0);

        sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(recover_abandoned(&pool).await.unwrap(), 1);
        // The crashed worker can no longer touch it...
        assert!(!heartbeat(&pool, job.id, "crashed", LOCK).await.unwrap());
        // ...and another worker picks it up.
        let again = claim(&pool, "w2", LOCK).await.unwrap().unwrap();
        assert_eq!((again.id, again.attempts), (job.id, 2));
        assert!(heartbeat(&pool, job.id, "w2", LOCK).await.unwrap());
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(10));
        assert_eq!(backoff(2), Duration::from_secs(20));
        assert_eq!(backoff(4), Duration::from_secs(80));
        assert_eq!(backoff(30), Duration::from_secs(3600));
    }
}
