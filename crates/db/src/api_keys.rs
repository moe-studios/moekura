//! Queries on `api_keys`. Like sessions, only a hash of each key is
//! stored; unlike sessions, keys don't slide: they last until revoked or
//! until their fixed expiry.

use std::time::Duration;

use sqlx::PgExecutor;
use time::OffsetDateTime;
use uwu_core::tokens::{NewToken, hash_token};

use crate::bans::ActiveBan;
use crate::users::User;

/// How often use of a key is recorded, at most.
pub const TOUCH_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Most keys one user may have.
pub const MAX_PER_USER: i64 = 20;

/// Longest key name.
pub const NAME_MAX_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ApiKey {
    pub id: i64,
    pub name: String,
    /// The key's first characters.
    pub prefix: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("You can have at most {MAX_PER_USER} API keys. Revoke one first.")]
    TooMany,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Creates a key for `user_id` and returns it; this is the only time the
/// key itself is available.
pub async fn create(
    db: &sqlx::PgPool,
    user_id: i64,
    name: &str,
    expires_at: Option<OffsetDateTime>,
) -> Result<String, CreateError> {
    let NewToken { token, hash } = NewToken::api_key();
    let mut tx = db.begin().await?;
    // Serialises creation per user, so the limit holds.
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_PER_USER {
        return Err(CreateError::TooMany);
    }
    sqlx::query(
        "INSERT INTO api_keys (user_id, name, token_hash, prefix, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(user_id)
    .bind(name)
    .bind(&hash[..])
    .bind(&token[..12])
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(token)
}

/// `user_id`'s keys, oldest first.
pub async fn list(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<ApiKey>> {
    sqlx::query_as(
        "SELECT id, name, prefix, created_at, last_used_at, expires_at
         FROM api_keys WHERE user_id = $1 ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Deletes one of `user_id`'s keys; false if they have no such key.
pub async fn revoke(db: impl PgExecutor<'_>, user_id: i64, id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM api_keys WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// A usable key and its (active) user.
#[derive(Debug, Clone)]
pub struct KeyUser {
    pub key_id: i64,
    pub last_used_at: Option<OffsetDateTime>,
    pub user: User,
    /// The user's ban in force, if any.
    pub ban: Option<ActiveBan>,
}

impl KeyUser {
    pub fn needs_touch(&self, now: OffsetDateTime) -> bool {
        self.last_used_at
            .is_none_or(|last| now - last >= TOUCH_INTERVAL)
    }
}

/// The key `token`, if it exists, hasn't expired, and its user is active.
pub async fn lookup(db: impl PgExecutor<'_>, token: &str) -> sqlx::Result<Option<KeyUser>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        key_id: i64,
        last_used_at: Option<OffsetDateTime>,
        #[sqlx(flatten)]
        user: User,
        banned: bool,
        ban_reason: Option<String>,
        ban_expires_at: Option<OffsetDateTime>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT k.id AS key_id, k.last_used_at,
                u.id, u.name::text, u.email::text, u.role_id, u.status, u.created_at, u.last_seen_at, u.settings,
                b.id IS NOT NULL AS banned, b.reason AS ban_reason, b.expires_at AS ban_expires_at
         FROM api_keys k JOIN users u ON u.id = k.user_id
         LEFT JOIN LATERAL (
             SELECT id, reason, expires_at FROM bans
             WHERE user_id = u.id AND lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
             ORDER BY expires_at DESC NULLS FIRST LIMIT 1
         ) b ON true
         WHERE k.token_hash = $1 AND (k.expires_at IS NULL OR k.expires_at > now())
           AND u.status = 'active'",
    )
    .bind(&hash_token(token)[..])
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| KeyUser {
        key_id: r.key_id,
        last_used_at: r.last_used_at,
        user: r.user,
        ban: r.banned.then(|| ActiveBan {
            reason: r.ban_reason.unwrap_or_default(),
            expires_at: r.ban_expires_at,
        }),
    }))
}

/// Records use of a key, and of its user.
pub async fn touch(db: impl PgExecutor<'_>, key_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "WITH k AS (UPDATE api_keys SET last_used_at = now() WHERE id = $1 RETURNING user_id)
         UPDATE users SET last_seen_at = now() FROM k WHERE users.id = k.user_id",
    )
    .bind(key_id)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uwu_core::permissions::SystemRole;

    use super::*;
    use crate::roles;
    use crate::users::{self, NewUser, UserStatus};

    async fn user(db: &PgPool, name: &str) -> User {
        let role_id = roles::by_system(db, SystemRole::Member).await.unwrap().id;
        let new = NewUser {
            name,
            email: None,
            password_hash: None,
            role_id,
            status: UserStatus::Active,
        };
        users::insert(db, new).await.unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn keys_work_until_revoked_or_expired(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let key = create(&pool, alice.id, "script", None).await.unwrap();
        let found = lookup(&pool, &key).await.unwrap().unwrap();
        assert_eq!(found.user.id, alice.id);
        assert!(found.ban.is_none());
        assert!(found.needs_touch(OffsetDateTime::now_utc()));
        touch(&pool, found.key_id).await.unwrap();
        assert!(
            !lookup(&pool, &key)
                .await
                .unwrap()
                .unwrap()
                .needs_touch(OffsetDateTime::now_utc())
        );

        let listed = list(&pool, alice.id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].prefix, &key[..12]);
        assert!(listed[0].last_used_at.is_some());

        // Only the owner can revoke it.
        assert!(!revoke(&pool, bob.id, listed[0].id).await.unwrap());
        assert!(revoke(&pool, alice.id, listed[0].id).await.unwrap());
        assert!(lookup(&pool, &key).await.unwrap().is_none());

        let past = OffsetDateTime::now_utc() - time::Duration::hours(1);
        let expired = create(&pool, alice.id, "old", Some(past)).await.unwrap();
        assert!(lookup(&pool, &expired).await.unwrap().is_none());
        assert!(lookup(&pool, "uwu_nonsense").await.unwrap().is_none());

        let key = create(&pool, bob.id, "bot", None).await.unwrap();
        users::set_status(&pool, bob.id, UserStatus::Deactivated)
            .await
            .unwrap();
        assert!(lookup(&pool, &key).await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn keys_per_user_are_limited(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        for n in 0..MAX_PER_USER {
            create(&pool, alice.id, &format!("key {n}"), None)
                .await
                .unwrap();
        }
        assert!(matches!(
            create(&pool, alice.id, "one more", None).await,
            Err(CreateError::TooMany)
        ));
    }
}
