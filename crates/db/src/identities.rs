//! Accounts at an OpenID Connect provider linked to users, and logins
//! waiting for the provider (`user_identities`, `oidc_logins`).

use std::time::Duration;

use moekura_core::tokens::{NewToken, hash_token};
use sqlx::PgExecutor;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Identity {
    pub id: i64,
    pub issuer: String,
    pub subject: String,
    pub created_at: OffsetDateTime,
}

/// The user a provider account logs in as.
pub async fn user_for(
    db: impl PgExecutor<'_>,
    issuer: &str,
    subject: &str,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar("SELECT user_id FROM user_identities WHERE issuer = $1 AND subject = $2")
        .bind(issuer)
        .bind(subject)
        .fetch_optional(db)
        .await
}

pub async fn for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<Identity>> {
    sqlx::query_as(
        "SELECT id, issuer, subject, created_at FROM user_identities
         WHERE user_id = $1 ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// Links a provider account to `user_id`. False if it's already linked
/// to someone (them included).
pub async fn link(
    db: impl PgExecutor<'_>,
    user_id: i64,
    issuer: &str,
    subject: &str,
) -> sqlx::Result<bool> {
    let inserted = sqlx::query(
        "INSERT INTO user_identities (user_id, issuer, subject) VALUES ($1, $2, $3)
         ON CONFLICT (issuer, subject) DO NOTHING",
    )
    .bind(user_id)
    .bind(issuer)
    .bind(subject)
    .execute(db)
    .await?;
    Ok(inserted.rows_affected() == 1)
}

pub async fn unlink(db: impl PgExecutor<'_>, user_id: i64, id: i64) -> sqlx::Result<bool> {
    let deleted = sqlx::query("DELETE FROM user_identities WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(deleted.rows_affected() == 1)
}

/// A login sent to the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLogin<'a> {
    pub nonce: &'a str,
    pub code_verifier: &'a str,
    pub next: Option<&'a str>,
    pub link_user_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PendingLogin {
    pub nonce: String,
    pub code_verifier: String,
    pub next: Option<String>,
    pub link_user_id: Option<i64>,
}

/// Remembers a login for `ttl`; returns the `state` to send along.
pub async fn start_login(
    db: impl PgExecutor<'_>,
    login: NewLogin<'_>,
    ttl: Duration,
) -> sqlx::Result<String> {
    let state = NewToken::generate();
    sqlx::query(
        "INSERT INTO oidc_logins (state_hash, nonce, code_verifier, next, link_user_id, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))",
    )
    .bind(&state.hash[..])
    .bind(login.nonce)
    .bind(login.code_verifier)
    .bind(login.next)
    .bind(login.link_user_id)
    .bind(ttl.as_secs_f64())
    .execute(db)
    .await?;
    Ok(state.token)
}

/// The login `state` belongs to, used up, if it hasn't expired.
pub async fn finish_login(
    db: impl PgExecutor<'_>,
    state: &str,
) -> sqlx::Result<Option<PendingLogin>> {
    sqlx::query_as(
        "DELETE FROM oidc_logins WHERE state_hash = $1 AND expires_at > now()
         RETURNING nonce, code_verifier, next, link_user_id",
    )
    .bind(&hash_token(state)[..])
    .fetch_optional(db)
    .await
}

/// Removes logins nobody came back from. Returns how many.
pub async fn prune_logins(db: impl PgExecutor<'_>) -> sqlx::Result<u64> {
    let result = sqlx::query("DELETE FROM oidc_logins WHERE expires_at < now()")
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn links_and_logins(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let issuer = "https://sso.example.com";
        assert!(link(&pool, alice, issuer, "a1").await.unwrap());
        assert!(!link(&pool, bob, issuer, "a1").await.unwrap(), "taken");
        assert!(
            link(&pool, bob, "https://other.example.com", "a1")
                .await
                .unwrap()
        );
        assert_eq!(user_for(&pool, issuer, "a1").await.unwrap(), Some(alice));
        let linked = for_user(&pool, alice).await.unwrap();
        assert_eq!(linked.len(), 1);
        assert!(
            !unlink(&pool, bob, linked[0].id).await.unwrap(),
            "not theirs"
        );
        assert!(unlink(&pool, alice, linked[0].id).await.unwrap());
        assert_eq!(user_for(&pool, issuer, "a1").await.unwrap(), None);

        let login = NewLogin {
            nonce: "n",
            code_verifier: "v",
            next: Some("/tags"),
            link_user_id: Some(alice),
        };
        let state = start_login(&pool, login.clone(), Duration::from_secs(600))
            .await
            .unwrap();
        let pending = finish_login(&pool, &state).await.unwrap().unwrap();
        assert_eq!(
            (
                pending.nonce.as_str(),
                pending.next.as_deref(),
                pending.link_user_id
            ),
            ("n", Some("/tags"), Some(alice))
        );
        assert!(finish_login(&pool, &state).await.unwrap().is_none(), "once");
        let stale = start_login(&pool, login, Duration::ZERO).await.unwrap();
        assert!(finish_login(&pool, &stale).await.unwrap().is_none());
        assert_eq!(prune_logins(&pool).await.unwrap(), 1);
    }
}
