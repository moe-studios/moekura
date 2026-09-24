//! Queries on `site_settings`.

use moekura_core::settings::{SettingError, SiteSettings};
use serde_json::Value;
use sqlx::{PgExecutor, PgPool};

use crate::site_cache::CHANNEL;

pub async fn load(db: impl PgExecutor<'_>) -> sqlx::Result<SiteSettings> {
    let rows: Vec<(String, Value)> = sqlx::query_as("SELECT key, value FROM site_settings")
        .fetch_all(db)
        .await?;
    let (settings, skipped) = SiteSettings::from_rows(rows);
    for key in skipped {
        tracing::warn!(key, "ignoring unknown or invalid stored site setting");
    }
    Ok(settings)
}

#[derive(Debug, thiserror::Error)]
pub enum SetError {
    #[error(transparent)]
    Invalid(#[from] SettingError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Validates and stores one setting, then tells every node to reload.
pub async fn set(db: &PgPool, key: &str, value: Value) -> Result<SiteSettings, SetError> {
    let mut tx = db.begin().await?;
    let updated = load(&mut *tx).await?.with_value(key, value.clone())?;
    sqlx::query(
        "INSERT INTO site_settings (key, value) VALUES ($1, $2)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(key)
    .bind(value)
    .execute(&mut *tx)
    .await?;
    // Delivered only if the transaction commits.
    sqlx::query("SELECT pg_notify($1, 'settings')")
        .bind(CHANNEL)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use moekura_core::settings::RegistrationMode;
    use serde_json::json;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn defaults_when_empty(pool: PgPool) {
        assert_eq!(load(&pool).await.unwrap(), SiteSettings::default());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn set_persists_and_overwrites(pool: PgPool) {
        set(&pool, "registration_mode", json!("invite"))
            .await
            .unwrap();
        set(&pool, "registration_mode", json!("closed"))
            .await
            .unwrap();
        let loaded = load(&pool).await.unwrap();
        assert_eq!(loaded.registration_mode, RegistrationMode::Closed);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn set_rejects_invalid_values_without_storing(pool: PgPool) {
        let err = set(&pool, "registration_mode", json!("maybe"))
            .await
            .unwrap_err();
        assert!(matches!(err, SetError::Invalid(_)), "{err:?}");
        let err = set(&pool, "nope", json!(1)).await.unwrap_err();
        assert!(matches!(
            err,
            SetError::Invalid(SettingError::UnknownKey(_))
        ));
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM site_settings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
}
