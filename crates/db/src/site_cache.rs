//! An in-memory copy of site settings and roles, read on nearly every
//! request.
//!
//! Every node keeps its own copy. Writers send a `NOTIFY` on [`CHANNEL`]
//! in the same transaction as their change, and [`SiteCache::listen`]
//! reloads on each notification, so all nodes pick up changes within
//! moments without an external message bus.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use moekura_core::permissions::{Role, SystemRole};
use moekura_core::settings::SiteSettings;
use sqlx::PgPool;
use sqlx::postgres::PgListener;

use crate::{roles, settings};

pub const CHANNEL: &str = "moekura_site_cache";

const RECONNECT_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub struct SiteSnapshot {
    pub settings: SiteSettings,
    roles: Vec<Role>,
}

impl SiteSnapshot {
    pub fn new(settings: SiteSettings, roles: Vec<Role>) -> Self {
        Self { settings, roles }
    }

    /// Lowest rank first.
    pub fn roles(&self) -> &[Role] {
        &self.roles
    }

    pub fn role(&self, id: i32) -> Option<&Role> {
        self.roles.iter().find(|r| r.id == id)
    }

    pub fn system_role(&self, system: SystemRole) -> Option<&Role> {
        self.roles.iter().find(|r| r.system == Some(system))
    }
}

#[derive(Clone)]
pub struct SiteCache {
    current: Arc<RwLock<Arc<SiteSnapshot>>>,
}

impl SiteCache {
    pub async fn load(db: &PgPool) -> sqlx::Result<Self> {
        let snapshot = fetch(db).await?;
        Ok(Self::from_snapshot(snapshot))
    }

    pub fn from_snapshot(snapshot: SiteSnapshot) -> Self {
        Self {
            current: Arc::new(RwLock::new(Arc::new(snapshot))),
        }
    }

    /// The current snapshot. Cheap; holds no lock after returning.
    pub fn get(&self) -> Arc<SiteSnapshot> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub async fn reload(&self, db: &PgPool) -> sqlx::Result<()> {
        let snapshot = Arc::new(fetch(db).await?);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = snapshot;
        Ok(())
    }

    /// Reloads whenever a change is announced on [`CHANNEL`]. Runs until the
    /// task is dropped; reconnects on its own after connection loss.
    pub async fn listen(self, db: PgPool) {
        loop {
            let mut listener = match subscribe(&db).await {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::warn!(%error, "site cache listener could not connect; retrying");
                    tokio::time::sleep(RECONNECT_DELAY).await;
                    continue;
                }
            };
            // Changes made while we were not subscribed were not announced.
            self.reload_logged(&db).await;

            loop {
                match listener.try_recv().await {
                    Ok(Some(_)) => self.reload_logged(&db).await,
                    // The connection dropped; the next call reconnects and
                    // resubscribes, but anything sent meanwhile was missed.
                    Ok(None) => self.reload_logged(&db).await,
                    Err(error) => {
                        tracing::warn!(%error, "site cache listener failed; resubscribing");
                        tokio::time::sleep(RECONNECT_DELAY).await;
                        break;
                    }
                }
            }
        }
    }

    async fn reload_logged(&self, db: &PgPool) {
        match self.reload(db).await {
            Ok(()) => tracing::debug!("site cache reloaded"),
            Err(error) => tracing::warn!(%error, "could not reload site cache"),
        }
    }
}

async fn subscribe(db: &PgPool) -> sqlx::Result<PgListener> {
    let mut listener = PgListener::connect_with(db).await?;
    listener.listen(CHANNEL).await?;
    Ok(listener)
}

async fn fetch(db: &PgPool) -> sqlx::Result<SiteSnapshot> {
    let settings = settings::load(db).await?;
    let roles = roles::list(db).await?;
    for system in SystemRole::ALL {
        if !roles.iter().any(|r| r.system == Some(system)) {
            tracing::error!(
                role = system.key(),
                "built-in role is missing from the database"
            );
        }
    }
    Ok(SiteSnapshot::new(settings, roles))
}

#[cfg(test)]
mod tests {
    use moekura_core::settings::RegistrationMode;
    use serde_json::json;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn loads_settings_and_roles(pool: PgPool) {
        let cache = SiteCache::load(&pool).await.unwrap();
        let snapshot = cache.get();
        assert_eq!(snapshot.settings, SiteSettings::default());
        let anonymous = snapshot.system_role(SystemRole::Anonymous).unwrap();
        assert_eq!(snapshot.role(anonymous.id), Some(anonymous));
        assert_eq!(snapshot.roles().len(), SystemRole::ALL.len());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn listener_picks_up_changes(pool: PgPool) {
        let cache = SiteCache::load(&pool).await.unwrap();
        // No wait for the subscription: a change that lands first is still
        // picked up by the reload that follows subscribing.
        let task = tokio::spawn(cache.clone().listen(pool.clone()));
        settings::set(&pool, "registration_mode", json!("closed"))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while cache.get().settings.registration_mode != RegistrationMode::Closed {
            assert!(
                tokio::time::Instant::now() < deadline,
                "cache never reloaded"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        task.abort();
    }
}
