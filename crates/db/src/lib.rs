//! PostgreSQL access for uwuubooru.

pub mod accounts;
pub mod bans;
pub mod favorites;
pub mod flags;
pub mod invites;
pub mod jobs;
pub mod media;
pub mod mod_actions;
pub mod post_versions;
pub mod posts;
pub mod roles;
pub mod search;
pub mod sessions;
pub mod settings;
pub mod site_cache;
pub mod tag_relations;
pub mod tags;
pub mod users;

#[cfg(test)]
mod schema_tests;

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use uwuu_core::config::DatabaseConfig;

/// Migrations embedded from `crates/db/migrations`.
pub static MIGRATOR: Migrator = sqlx::migrate!();

/// Connection pools for the primary and any read replicas.
///
/// Writes, and reads that must see them, go through [`Db::primary`].
/// Reads that can tolerate replication lag (listings, search, tag pages) go
/// through [`Db::read`].
#[derive(Clone)]
pub struct Db {
    primary: PgPool,
    replicas: Arc<[PgPool]>,
    next_replica: Arc<AtomicUsize>,
}

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
            primary,
            replicas: replicas.into(),
            next_replica: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    /// A pool for replica-safe reads, round-robin across replicas, or the
    /// primary when none are configured.
    pub fn read(&self) -> &PgPool {
        if self.replicas.is_empty() {
            return &self.primary;
        }
        let i = self.next_replica.fetch_add(1, Ordering::Relaxed) % self.replicas.len();
        &self.replicas[i]
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
    let mut options = PgConnectOptions::from_str(url)?.application_name("uwuubooru");
    if config.statement_timeout_ms > 0 {
        options = options.options([("statement_timeout", config.statement_timeout_ms.to_string())]);
    }
    Ok(options)
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
