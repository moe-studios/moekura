//! Background job workers.
//!
//! [`run`] starts a pool of workers that claim jobs from the Postgres queue
//! ([`moekura_db::jobs`]) and hand them to the handler registered for their
//! kind in a [`Registry`]. Workers only claim kinds they have a handler
//! for, so processes with different registries (`moekura tagger`) share
//! the queue. Idle workers wake on `NOTIFY` rather than
//! polling hard, a reaper requeues jobs whose worker vanished, and shutdown
//! stops claiming but lets running jobs finish. Jobs registered with
//! [`Registry::every`] are enqueued on a schedule, once across all nodes.

use std::collections::HashMap;
use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use moekura_core::jobs::Job;
use moekura_db::jobs::{self, ClaimedJob};
use serde_json::Value;
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::Instrument;

pub mod mail;
pub mod media;
pub mod posts;
pub mod stats;
pub mod tags;
pub mod users;
pub mod versions;
pub mod webhooks;

/// Why a job failed.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// Might work later (a timeout, a database hiccup): retried with backoff.
    #[error("{0}")]
    Retry(String),
    /// Might work after the given wait (a receiver's `Retry-After`).
    #[error("{0}")]
    RetryIn(String, Duration),
    /// Will never work (bad input): marked dead immediately.
    #[error("{0}")]
    Permanent(String),
}

impl JobError {
    pub fn retry(error: impl Display) -> Self {
        Self::Retry(error.to_string())
    }

    pub fn permanent(error: impl Display) -> Self {
        Self::Permanent(error.to_string())
    }
}

impl From<sqlx::Error> for JobError {
    fn from(error: sqlx::Error) -> Self {
        Self::retry(format!("database: {error}"))
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = Result<(), JobError>> + Send>>;
type Handler = Arc<dyn Fn(Value) -> BoxFuture + Send + Sync>;
type Enqueue =
    Arc<dyn Fn(PgPool) -> Pin<Box<dyn Future<Output = sqlx::Result<bool>> + Send>> + Send + Sync>;

/// A job enqueued every `every`.
#[derive(Clone)]
struct Scheduled {
    kind: &'static str,
    every: Duration,
    enqueue: Enqueue,
}

/// Maps job kinds to handlers, and knows which jobs run on a schedule and
/// which may only run a few at a time.
#[derive(Default, Clone)]
pub struct Registry {
    handlers: HashMap<&'static str, Handler>,
    scheduled: Vec<Scheduled>,
    /// Kinds with the most of their jobs that may run at once.
    limits: Vec<(&'static str, i32)>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles jobs of type `J` with `handler`.
    pub fn register<J, F, Fut>(&mut self, handler: F) -> &mut Self
    where
        J: Job,
        F: Fn(J) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), JobError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        self.handlers.insert(
            J::KIND,
            Arc::new(move |payload: Value| -> BoxFuture {
                let handler = handler.clone();
                Box::pin(async move {
                    let job: J = serde_json::from_value(payload)
                        .map_err(|e| JobError::permanent(format!("invalid payload: {e}")))?;
                    handler(job).await
                })
            }),
        );
        self
    }

    /// The job kinds this registry handles.
    pub fn kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<&'static str> = self.handlers.keys().copied().collect();
        kinds.sort_unstable();
        kinds
    }

    /// Runs at most `most` jobs of type `J` at once, across all workers and
    /// nodes, so jobs that wait on something slow can't hold every worker.
    pub fn at_most<J: Job>(&mut self, most: u32) -> &mut Self {
        self.limits.retain(|(kind, _)| *kind != J::KIND);
        self.limits
            .push((J::KIND, i32::try_from(most).unwrap_or(i32::MAX)));
        self
    }

    /// Enqueues a `J` every `every` (and shortly after start), unless one
    /// is already waiting or running on any node.
    pub fn every<J: Job + Default + Sync>(&mut self, every: Duration) -> &mut Self {
        self.scheduled.push(Scheduled {
            kind: J::KIND,
            every,
            enqueue: Arc::new(|db: PgPool| {
                Box::pin(async move { jobs::enqueue_unique(&db, &J::default()).await })
            }),
        });
        self
    }
}

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub workers: usize,
    /// How long a claimed job stays locked without a heartbeat.
    pub lock: Duration,
    /// Poll interval when no notification arrives: picks up retries whose
    /// backoff ended and covers a lost `LISTEN` connection.
    pub idle_poll: Duration,
    /// How often abandoned jobs are looked for.
    pub reap_every: Duration,
}

impl PoolConfig {
    pub fn new(workers: usize, lock: Duration) -> Self {
        Self {
            workers,
            lock,
            idle_poll: Duration::from_secs(10),
            reap_every: Duration::from_secs(60),
        }
    }
}

/// Runs workers until `shutdown` is cancelled, then waits for jobs in
/// progress to finish.
pub async fn run(db: PgPool, registry: Registry, config: PoolConfig, shutdown: CancellationToken) {
    let wake = Arc::new(Notify::new());
    let kinds = Arc::new(registry.kinds());
    let registry = Arc::new(registry);
    let prefix = worker_prefix();
    tracing::info!(workers = config.workers, "job workers started");

    let mut background = vec![
        tokio::spawn(listen(db.clone(), wake.clone())),
        tokio::spawn(reap(db.clone(), config.reap_every)),
    ];
    for scheduled in registry.scheduled.iter().cloned() {
        background.push(tokio::spawn(schedule(db.clone(), scheduled)));
    }

    let tracker = TaskTracker::new();
    for n in 0..config.workers {
        let worker = Worker {
            id: format!("{prefix}-{n}"),
            db: db.clone(),
            registry: registry.clone(),
            kinds: kinds.clone(),
            wake: wake.clone(),
            config: config.clone(),
        };
        tracker.spawn(worker.run(shutdown.clone()));
    }
    tracker.close();
    tracker.wait().await;

    for task in background {
        task.abort();
    }
    tracing::info!("job workers stopped");
}

struct Worker {
    id: String,
    db: PgPool,
    registry: Arc<Registry>,
    kinds: Arc<Vec<&'static str>>,
    wake: Arc<Notify>,
    config: PoolConfig,
}

impl Worker {
    async fn run(self, shutdown: CancellationToken) {
        while !shutdown.is_cancelled() {
            // Register interest before looking, so a job enqueued between
            // an empty claim and the wait still wakes us.
            let notified = self.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let claimed = jobs::claim_limited(
                &self.db,
                &self.id,
                self.config.lock,
                &self.kinds,
                &self.registry.limits,
            );
            match claimed.await {
                Ok(Some(job)) => {
                    let span = tracing::info_span!(
                        "job",
                        id = job.id,
                        kind = job.kind,
                        attempt = job.attempts
                    );
                    self.execute(job).instrument(span).await;
                }
                Ok(None) => {
                    tokio::select! {
                        () = &mut notified => {}
                        () = tokio::time::sleep(self.config.idle_poll) => {}
                        () = shutdown.cancelled() => {}
                    }
                }
                Err(error) => {
                    tracing::warn!(worker = self.id, %error, "could not claim a job");
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(5)) => {}
                        () = shutdown.cancelled() => {}
                    }
                }
            }
        }
    }

    async fn execute(&self, job: ClaimedJob) {
        let started = tokio::time::Instant::now();

        let Some(handler) = self.registry.handlers.get(job.kind.as_str()).cloned() else {
            let error = format!("no handler for job kind `{}`", job.kind);
            tracing::error!(error);
            self.record_failure(&job, &error, true).await;
            return;
        };

        // Its own task, so a panic fails this attempt instead of the worker.
        let mut work = tokio::spawn(handler(job.payload.clone()).in_current_span());
        let mut heartbeat = tokio::time::interval(self.config.lock / 3);
        heartbeat.tick().await;
        let outcome = loop {
            tokio::select! {
                result = &mut work => break result,
                _ = heartbeat.tick() => {
                    if let Err(error) = jobs::heartbeat(&self.db, job.id, &self.id, self.config.lock).await {
                        tracing::warn!(%error, "could not extend job lock");
                    }
                }
            }
        };

        let elapsed = started.elapsed();
        let result = match &outcome {
            Ok(Ok(())) => "done",
            Ok(Err(JobError::Retry(_) | JobError::RetryIn(..))) => "retry",
            Ok(Err(JobError::Permanent(_))) => "failed",
            Err(_) => "panicked",
        };
        // Kinds are the registry's, so the labels are a fixed set.
        metrics::counter!("moekura_jobs_finished_total", "kind" => job.kind.clone(), "result" => result)
            .increment(1);
        metrics::histogram!("moekura_job_duration_seconds", "kind" => job.kind.clone())
            .record(elapsed.as_secs_f64());
        match outcome {
            Ok(Ok(())) => {
                tracing::debug!(?elapsed, "job done");
                if let Err(error) = jobs::complete(&self.db, job.id, &self.id).await {
                    tracing::warn!(%error, "could not mark job done; it may run again");
                }
            }
            Ok(Err(JobError::Retry(error))) => {
                tracing::warn!(?elapsed, error, "job failed");
                self.record_failure(&job, &error, false).await;
            }
            Ok(Err(JobError::RetryIn(error, wait))) => {
                tracing::warn!(?elapsed, error, ?wait, "job failed; asked to wait");
                self.record_failure_in(&job, &error, wait).await;
            }
            Ok(Err(JobError::Permanent(error))) => {
                tracing::error!(?elapsed, error, "job failed permanently");
                self.record_failure(&job, &error, true).await;
            }
            Err(join_error) => {
                let error = format!("job panicked: {join_error}");
                tracing::error!(?elapsed, error);
                self.record_failure(&job, &error, false).await;
            }
        }
    }

    async fn record_failure(&self, job: &ClaimedJob, error: &str, permanent: bool) {
        let retry_in = jobs::backoff(job.attempts);
        if let Err(db_error) =
            jobs::fail(&self.db, job.id, &self.id, error, retry_in, permanent).await
        {
            tracing::warn!(%db_error, "could not record job failure; it will be retried after its lock expires");
        }
    }

    /// Like a failure to retry, but after `wait` rather than the backoff.
    async fn record_failure_in(&self, job: &ClaimedJob, error: &str, wait: Duration) {
        if let Err(db_error) = jobs::fail(&self.db, job.id, &self.id, error, wait, false).await {
            tracing::warn!(%db_error, "could not record job failure; it will be retried after its lock expires");
        }
    }
}

/// Wakes idle workers when jobs are enqueued anywhere.
async fn listen(db: PgPool, wake: Arc<Notify>) {
    loop {
        let mut listener = match PgListener::connect_with(&db).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, "job listener could not connect; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        if let Err(error) = listener.listen(jobs::CHANNEL).await {
            tracing::warn!(%error, "job listener could not subscribe; retrying");
            tokio::time::sleep(Duration::from_secs(5)).await;
            continue;
        }
        loop {
            match listener.try_recv().await {
                // A lost connection may have swallowed notifications; waking
                // everyone is harmless.
                Ok(_) => wake.notify_waiters(),
                Err(error) => {
                    tracing::warn!(%error, "job listener failed; resubscribing");
                    break;
                }
            }
        }
    }
}

async fn schedule(db: PgPool, job: Scheduled) {
    let mut interval = tokio::time::interval(job.every);
    // A missed tick (a long pause) runs once, not in a burst.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        match (job.enqueue)(db.clone()).await {
            Ok(true) => tracing::debug!(kind = job.kind, "scheduled job enqueued"),
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, kind = job.kind, "could not enqueue a scheduled job")
            }
        }
    }
}

async fn reap(db: PgPool, every: Duration) {
    let mut interval = tokio::time::interval(every);
    loop {
        interval.tick().await;
        match jobs::recover_abandoned(&db).await {
            Ok(0) => {}
            Ok(recovered) => {
                tracing::warn!(recovered, "requeued jobs whose worker stopped responding")
            }
            Err(error) => tracing::warn!(%error, "could not check for abandoned jobs"),
        }
    }
}

/// `host-pid`, so the owner of a locked job can be traced.
fn worker_prefix() -> String {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "worker".to_owned());
    format!("{host}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Serialize, Deserialize)]
    struct Count {
        fail_times: usize,
    }

    impl Job for Count {
        const KIND: &'static str = "test.count";
        const MAX_ATTEMPTS: i32 = 3;
    }

    #[derive(Serialize, Deserialize)]
    struct Panic;

    impl Job for Panic {
        const KIND: &'static str = "test.panic";
        const MAX_ATTEMPTS: i32 = 1;
    }

    #[derive(Serialize, Deserialize)]
    struct Slow;

    impl Job for Slow {
        const KIND: &'static str = "test.slow";
    }

    fn fast_config(workers: usize) -> PoolConfig {
        PoolConfig {
            workers,
            lock: Duration::from_secs(30),
            idle_poll: Duration::from_millis(50),
            reap_every: Duration::from_secs(60),
        }
    }

    async fn push<J: Job>(pool: &PgPool, job: &J) {
        let mut conn = pool.acquire().await.unwrap();
        jobs::enqueue(&mut conn, job).await.unwrap();
    }

    async fn wait_until(what: &str, mut check: impl AsyncFnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !check().await {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn runs_jobs_and_retries_failures(pool: PgPool) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = Registry::new();
        let seen = calls.clone();
        registry.register(move |job: Count| {
            let seen = seen.clone();
            async move {
                let call = seen.fetch_add(1, Ordering::SeqCst);
                if call < job.fail_times {
                    Err(JobError::retry("not yet"))
                } else {
                    Ok(())
                }
            }
        });

        let shutdown = CancellationToken::new();
        let pool_task = tokio::spawn(run(
            pool.clone(),
            registry,
            fast_config(2),
            shutdown.clone(),
        ));

        push(&pool, &Count { fail_times: 1 }).await;
        // The retry waits out its backoff; skip it.
        // Wait for the worker to record the failure (not just for the handler
        // to run), or its backoff would overwrite the run_at set below.
        wait_until("the first failure to be recorded", async || {
            let status: Option<String> =
                sqlx::query_scalar("SELECT status FROM jobs WHERE last_error IS NOT NULL")
                    .fetch_optional(&pool)
                    .await
                    .unwrap();
            status.as_deref() == Some("queued")
        })
        .await;
        sqlx::query("UPDATE jobs SET run_at = now()")
            .execute(&pool)
            .await
            .unwrap();
        wait_until("the job to finish", async || {
            jobs::counts(&pool).await.unwrap()
                == jobs::JobCounts {
                    queued: 0,
                    running: 0,
                    dead: 0,
                }
        })
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        shutdown.cancel();
        pool_task.await.unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn panics_fail_the_job_not_the_worker(pool: PgPool) {
        let mut registry = Registry::new();
        registry.register(|_: Panic| async { panic!("handler exploded") });
        registry.register(|_: Count| async { Ok(()) });

        let shutdown = CancellationToken::new();
        let pool_task = tokio::spawn(run(
            pool.clone(),
            registry,
            fast_config(1),
            shutdown.clone(),
        ));

        push(&pool, &Panic).await;
        // No handler here: left for a process that has one.
        push(&pool, &Slow).await;
        push(&pool, &Count { fail_times: 0 }).await;

        wait_until("a dead job and the good one done", async || {
            jobs::counts(&pool).await.unwrap()
                == jobs::JobCounts {
                    queued: 1,
                    running: 0,
                    dead: 1,
                }
        })
        .await;
        let error: String = sqlx::query_scalar("SELECT last_error FROM jobs WHERE status = 'dead'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(error.contains("panicked"), "{error}");
        let waiting: String = sqlx::query_scalar("SELECT kind FROM jobs WHERE status = 'queued'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(waiting, "test.slow");

        shutdown.cancel();
        pool_task.await.unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn shutdown_lets_running_jobs_finish(pool: PgPool) {
        let finished = Arc::new(AtomicUsize::new(0));
        let mut registry = Registry::new();
        let done = finished.clone();
        registry.register(move |_: Slow| {
            let done = done.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                done.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });

        let shutdown = CancellationToken::new();
        let pool_task = tokio::spawn(run(
            pool.clone(),
            registry,
            fast_config(1),
            shutdown.clone(),
        ));
        push(&pool, &Slow).await;
        wait_until("the job to start", async || {
            jobs::counts(&pool).await.unwrap().running == 1
        })
        .await;

        shutdown.cancel();
        pool_task.await.unwrap();
        assert_eq!(finished.load(Ordering::SeqCst), 1);
        assert_eq!(jobs::counts(&pool).await.unwrap().running, 0);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn limited_kinds_leave_workers_free(pool: PgPool) {
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let counted = Arc::new(AtomicUsize::new(0));
        let mut registry = Registry::new();
        let held = gate.clone();
        registry
            .register(move |_: Slow| {
                let held = held.clone();
                async move {
                    held.acquire().await.unwrap().forget();
                    Ok(())
                }
            })
            .at_most::<Slow>(1);
        let seen = counted.clone();
        registry.register(move |_: Count| {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        push(&pool, &Slow).await;
        push(&pool, &Slow).await;
        push(&pool, &Count { fail_times: 0 }).await;

        let shutdown = CancellationToken::new();
        let pool_task = tokio::spawn(run(
            pool.clone(),
            registry,
            fast_config(2),
            shutdown.clone(),
        ));
        // One slow job at a time, so the other worker gets to the rest.
        wait_until("the other kind to run", async || {
            counted.load(Ordering::SeqCst) == 1
        })
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(jobs::counts(&pool).await.unwrap().running, 1);

        gate.add_permits(2);
        wait_until("the slow jobs to finish", async || {
            jobs::counts(&pool).await.unwrap()
                == jobs::JobCounts {
                    queued: 0,
                    running: 0,
                    dead: 0,
                }
        })
        .await;
        shutdown.cancel();
        pool_task.await.unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn wakes_on_notify_without_polling(pool: PgPool) {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = Registry::new();
        let seen = calls.clone();
        registry.register(move |_: Count| {
            let seen = seen.clone();
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        });
        // Polling far slower than the test timeout: only NOTIFY can wake it.
        let config = PoolConfig {
            idle_poll: Duration::from_secs(3600),
            ..fast_config(1)
        };
        let shutdown = CancellationToken::new();
        let pool_task = tokio::spawn(run(pool.clone(), registry, config, shutdown.clone()));
        // Let the worker go idle and the listener subscribe.
        tokio::time::sleep(Duration::from_millis(300)).await;

        push(&pool, &Count { fail_times: 0 }).await;
        wait_until("the notified worker to run the job", async || {
            calls.load(Ordering::SeqCst) == 1
        })
        .await;

        shutdown.cancel();
        pool_task.await.unwrap();
    }
}
