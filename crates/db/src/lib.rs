//! PostgreSQL access for Moekura.

pub mod account_tokens;
pub mod accounts;
pub mod api_keys;
pub mod bans;
pub mod bench;
pub mod comments;
pub mod favorite_groups;
pub mod favorites;
pub mod flags;
pub mod identities;
pub mod invites;
pub mod jobs;
pub mod media;
pub mod mod_actions;
pub mod notes;
pub mod pools;
pub mod post_versions;
pub mod posts;
pub mod roles;
pub mod saved_searches;
pub mod search;
pub mod secrets;
pub mod seed;
pub mod sessions;
pub mod settings;
pub mod site_cache;
pub mod stats;
pub mod tag_relations;
pub mod tags;
pub mod two_factor;
pub mod users;
pub mod wiki;

#[cfg(test)]
mod schema_tests;

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use moekura_core::config::DatabaseConfig;
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// Migrations embedded from `crates/db/migrations`.
pub static MIGRATOR: Migrator = sqlx::migrate!();

/// Connection pools for the primary and any read replicas.
///
/// Writes, and reads that must see them, go through [`Db::primary`].
/// Reads that can tolerate replication lag (listings, search, tag pages) go
/// through [`Db::read`], which skips replicas [`Db::check_replicas`] found
/// unreachable or too far behind.
#[derive(Clone)]
pub struct Db {
    primary: PgPool,
    replicas: Arc<[PgPool]>,
    /// Per replica: whether reads may use it. Replicas start usable, so a
    /// fresh process spreads reads before the first check.
    usable: Arc<[AtomicBool]>,
    next_replica: Arc<AtomicUsize>,
}

/// How a replica was found by [`Db::check_replicas`].
#[derive(Debug, Clone, PartialEq)]
pub enum ReplicaState {
    /// Seconds behind the primary; 0 when it has replayed everything it
    /// received.
    Lagging(f64),
    Unreachable(String),
}

/// Checks run with this timeout, so a hung replica doesn't hold up the
/// others.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

impl Db {
    /// Connects to the primary, failing if it is unreachable. Replica pools
    /// connect lazily so a replica outage does not block startup.
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, sqlx::Error> {
        let primary = pool_options(config)
            .connect_with(connect_options(&config.url, config)?)
            .await?;
        let replicas = config
            .replicas
            .iter()
            .map(|url| Ok(pool_options(config).connect_lazy_with(connect_options(url, config)?)))
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        Ok(Self::from_pools(primary, replicas))
    }

    pub fn from_pools(primary: PgPool, replicas: Vec<PgPool>) -> Self {
        Self {
            usable: replicas.iter().map(|_| AtomicBool::new(true)).collect(),
            primary,
            replicas: replicas.into(),
            next_replica: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn has_replicas(&self) -> bool {
        !self.replicas.is_empty()
    }

    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    /// A pool for replica-safe reads: round-robin across the usable
    /// replicas, or the primary when there are none.
    pub fn read(&self) -> &PgPool {
        let count = self.replicas.len();
        if count == 0 {
            return &self.primary;
        }
        let start = self.next_replica.fetch_add(1, Ordering::Relaxed);
        (0..count)
            .map(|offset| (start + offset) % count)
            .find(|&i| self.usable[i].load(Ordering::Relaxed))
            .map_or(&self.primary, |i| &self.replicas[i])
    }

    /// Checks every replica, marking it usable when it answers and is at
    /// most `max_lag` behind. Returns what was found, in configuration
    /// order.
    pub async fn check_replicas(&self, max_lag: Duration) -> Vec<ReplicaState> {
        let mut states = Vec::with_capacity(self.replicas.len());
        for (replica, usable) in self.replicas.iter().zip(self.usable.iter()) {
            // A replica that has replayed all it received is caught up, even
            // if nothing has been written for a while (the last replayed
            // transaction then looks old).
            let lag = sqlx::query_scalar::<_, f64>(
                "SELECT CASE
                     WHEN NOT pg_is_in_recovery() THEN 0
                     WHEN pg_last_wal_receive_lsn() = pg_last_wal_replay_lsn() THEN 0
                     ELSE coalesce(extract(epoch FROM now() - pg_last_xact_replay_timestamp()), 0)
                 END::float8",
            )
            .fetch_one(replica);
            let state = match tokio::time::timeout(CHECK_TIMEOUT, lag).await {
                Ok(Ok(seconds)) => ReplicaState::Lagging(seconds),
                Ok(Err(error)) => ReplicaState::Unreachable(error.to_string()),
                Err(_) => ReplicaState::Unreachable("timed out".into()),
            };
            let ok = matches!(state, ReplicaState::Lagging(s) if s <= max_lag.as_secs_f64());
            usable.store(ok, Ordering::Relaxed);
            states.push(state);
        }
        states
    }

    /// Checks the replicas every few seconds, forever, logging when one
    /// stops or starts being used. Run it in a background task.
    pub async fn monitor_replicas(self, max_lag: Duration) {
        if self.replicas.is_empty() {
            return;
        }
        let mut was_usable: Vec<bool> = vec![true; self.replicas.len()];
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let states = self.check_replicas(max_lag).await;
            for (i, state) in states.iter().enumerate() {
                let usable = self.usable[i].load(Ordering::Relaxed);
                if usable != was_usable[i] {
                    match (usable, state) {
                        (true, _) => tracing::info!(replica = i, "replica back in use"),
                        (false, ReplicaState::Lagging(seconds)) => {
                            tracing::warn!(
                                replica = i,
                                lag_secs = seconds,
                                "replica is behind; reading from others"
                            )
                        }
                        (false, ReplicaState::Unreachable(error)) => {
                            tracing::warn!(replica = i, %error, "replica unreachable; reading from others")
                        }
                    }
                    was_usable[i] = usable;
                }
            }
        }
    }

    /// Applies pending migrations to the primary. Safe to run from several
    /// processes at once; sqlx serialises them with an advisory lock.
    pub async fn migrate(&self) -> Result<(), MigrateError> {
        MIGRATOR.run(&self.primary).await
    }

    /// Round-trips a trivial query on the primary.
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1")
            .execute(&self.primary)
            .await
            .map(|_| ())
    }

    pub async fn close(&self) {
        self.primary.close().await;
        for replica in self.replicas.iter() {
            replica.close().await;
        }
    }
}

fn pool_options(config: &DatabaseConfig) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(config.max_connections)
        .min_connections(config.min_connections)
        .acquire_timeout(Duration::from_secs(config.acquire_timeout_secs))
}

fn connect_options(url: &str, config: &DatabaseConfig) -> Result<PgConnectOptions, sqlx::Error> {
    // Queries are prepared, and after a few runs PostgreSQL may switch to
    // a generic plan that ignores the parameters. For searches that's
    // ruinous: a plan that suits a rare tag scans millions of posts for a
    // common one. Planning each time costs a fraction of a millisecond.
    let mut settings = vec![("plan_cache_mode", "force_custom_plan".to_owned())];
    if config.statement_timeout_ms > 0 {
        settings.push(("statement_timeout", config.statement_timeout_ms.to_string()));
    }
    Ok(PgConnectOptions::from_str(url)?
        .application_name("moekura")
        .options(settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lazy_pool() -> PgPool {
        PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .unwrap()
    }

    #[tokio::test]
    async fn read_uses_primary_without_replicas() {
        let db = Db::from_pools(lazy_pool(), vec![]);
        assert!(std::ptr::eq(db.read(), db.primary()));
    }

    #[tokio::test]
    async fn read_round_robins_replicas() {
        let db = Db::from_pools(lazy_pool(), vec![lazy_pool(), lazy_pool()]);
        let first: *const PgPool = db.read();
        let second: *const PgPool = db.read();
        let third: *const PgPool = db.read();
        assert!(!std::ptr::eq(first, db.primary()));
        assert!(!std::ptr::eq(first, second));
        assert!(std::ptr::eq(first, third));
    }

    /// A pool to a port nothing listens on.
    fn dead_pool() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(500))
            .connect_lazy("postgres://moekura@127.0.0.1:1/moekura")
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn unusable_replicas_are_skipped(pool: PgPool) {
        // The primary stands in for a caught-up replica.
        let db = Db::from_pools(pool.clone(), vec![dead_pool(), pool.clone()]);
        let states = db.check_replicas(Duration::from_secs(10)).await;
        assert!(
            matches!(states[0], ReplicaState::Unreachable(_)),
            "{states:?}"
        );
        assert_eq!(states[1], ReplicaState::Lagging(0.0));
        for _ in 0..4 {
            assert!(std::ptr::eq(db.read(), &db.replicas[1]));
        }

        let db = Db::from_pools(pool, vec![dead_pool()]);
        db.check_replicas(Duration::from_secs(10)).await;
        assert!(std::ptr::eq(db.read(), db.primary()));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn migrations_install_extensions(pool: PgPool) {
        let installed: Vec<String> = sqlx::query_scalar(
            "SELECT extname::text FROM pg_extension WHERE extname IN ('citext', 'intarray', 'pg_trgm') ORDER BY 1",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(installed, ["citext", "intarray", "pg_trgm"]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn ping_succeeds(pool: PgPool) {
        Db::from_pools(pool, vec![]).ping().await.unwrap();
    }
}
