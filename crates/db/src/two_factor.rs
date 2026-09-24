//! Two-factor login: TOTP secrets, recovery codes, and logins waiting for
//! a code (`user_totp`, `user_recovery_codes`, `login_challenges`).

use std::time::Duration;

use moekura_core::tokens::{NewToken, TokenHash, hash_token};
use moekura_core::totp::Secret;
use sqlx::{PgConnection, PgExecutor};

#[derive(Debug, Clone)]
pub struct Totp {
    pub secret: Secret,
    /// False while setting up.
    pub enabled: bool,
    pub last_step: i64,
}

pub async fn get(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Option<Totp>> {
    let row: Option<(Vec<u8>, bool, i64)> = sqlx::query_as(
        "SELECT secret, enabled_at IS NOT NULL, last_step FROM user_totp WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(secret, enabled, last_step)| Totp {
        secret: Secret::from_bytes(secret),
        enabled,
        last_step,
    }))
}

/// Whether the user must give a code to log in.
pub async fn is_enabled(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_totp WHERE user_id = $1 AND enabled_at IS NOT NULL)",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

/// Which of `user_ids` have two-factor login on.
pub async fn enabled_among(db: impl PgExecutor<'_>, user_ids: &[i64]) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT user_id FROM user_totp WHERE user_id = ANY($1) AND enabled_at IS NOT NULL",
    )
    .bind(user_ids)
    .fetch_all(db)
    .await
}

/// Starts setting up with a new `secret`, replacing an unfinished setup.
/// Does nothing if two-factor login is already on.
pub async fn begin(db: impl PgExecutor<'_>, user_id: i64, secret: &Secret) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO user_totp (user_id, secret) VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE SET secret = excluded.secret, created_at = now()
         WHERE user_totp.enabled_at IS NULL",
    )
    .bind(user_id)
    .bind(secret.as_bytes())
    .execute(db)
    .await?;
    Ok(())
}

/// Finishes setting up, after `step`'s code proved the app has the
/// secret, with fresh recovery codes.
pub async fn enable(
    conn: &mut PgConnection,
    user_id: i64,
    step: i64,
    recovery_codes: &[TokenHash],
) -> sqlx::Result<()> {
    sqlx::query("UPDATE user_totp SET enabled_at = now(), last_step = $2 WHERE user_id = $1")
        .bind(user_id)
        .bind(step)
        .execute(&mut *conn)
        .await?;
    replace_recovery_codes(conn, user_id, recovery_codes).await
}

/// Records that `step`'s code was used, unless it (or a later one)
/// already was. False means the code was already used: refuse it.
pub async fn use_step(db: impl PgExecutor<'_>, user_id: i64, step: i64) -> sqlx::Result<bool> {
    let updated = sqlx::query(
        "UPDATE user_totp SET last_step = $2
         WHERE user_id = $1 AND enabled_at IS NOT NULL AND last_step < $2",
    )
    .bind(user_id)
    .bind(step)
    .execute(db)
    .await?;
    Ok(updated.rows_affected() == 1)
}

/// Uses up a recovery code, if it's one of the user's unused ones.
pub async fn use_recovery_code(
    db: impl PgExecutor<'_>,
    user_id: i64,
    hash: &TokenHash,
) -> sqlx::Result<bool> {
    let updated = sqlx::query(
        "UPDATE user_recovery_codes SET used_at = now()
         WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
    )
    .bind(user_id)
    .bind(&hash[..])
    .execute(db)
    .await?;
    Ok(updated.rows_affected() == 1)
}

pub async fn replace_recovery_codes(
    conn: &mut PgConnection,
    user_id: i64,
    hashes: &[TokenHash],
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *conn)
        .await?;
    let hashes: Vec<&[u8]> = hashes.iter().map(|h| &h[..]).collect();
    sqlx::query(
        "INSERT INTO user_recovery_codes (user_id, code_hash)
         SELECT $1, hash FROM unnest($2::bytea[]) AS hash",
    )
    .bind(user_id)
    .bind(hashes)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

pub async fn unused_recovery_codes(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM user_recovery_codes WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

/// Turns two-factor login off (or abandons setting it up). Returns
/// whether it was on.
pub async fn disable(conn: &mut PgConnection, user_id: i64) -> sqlx::Result<bool> {
    let enabled: Option<bool> = sqlx::query_scalar(
        "DELETE FROM user_totp WHERE user_id = $1 RETURNING enabled_at IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM user_recovery_codes WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *conn)
        .await?;
    Ok(enabled.unwrap_or(false))
}

/// A login waiting for a code.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Challenge {
    pub user_id: i64,
    pub next: Option<String>,
    pub attempts: i32,
}

/// Starts waiting for `user_id`'s code; returns the token for the cookie.
pub async fn challenge(
    db: impl PgExecutor<'_>,
    user_id: i64,
    next: Option<&str>,
    ttl: Duration,
) -> sqlx::Result<String> {
    let token = NewToken::generate();
    // Expired challenges are cleared by prune_challenges.
    sqlx::query(
        "INSERT INTO login_challenges (token_hash, user_id, next, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
    )
    .bind(&token.hash[..])
    .bind(user_id)
    .bind(next)
    .bind(ttl.as_secs_f64())
    .execute(db)
    .await?;
    Ok(token.token)
}

/// The login waiting behind `token`, counting this as an attempt.
pub async fn attempt(db: impl PgExecutor<'_>, token: &str) -> sqlx::Result<Option<Challenge>> {
    sqlx::query_as(
        "UPDATE login_challenges SET attempts = attempts + 1
         WHERE token_hash = $1 AND expires_at > now()
         RETURNING user_id, next, attempts",
    )
    .bind(&hash_token(token)[..])
    .fetch_optional(db)
    .await
}

/// The login waiting behind `token`, without counting an attempt.
pub async fn pending(db: impl PgExecutor<'_>, token: &str) -> sqlx::Result<Option<Challenge>> {
    sqlx::query_as(
        "SELECT user_id, next, attempts FROM login_challenges
         WHERE token_hash = $1 AND expires_at > now()",
    )
    .bind(&hash_token(token)[..])
    .fetch_optional(db)
    .await
}

pub async fn end_challenge(db: impl PgExecutor<'_>, token: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM login_challenges WHERE token_hash = $1")
        .bind(&hash_token(token)[..])
        .execute(db)
        .await?;
    Ok(())
}

/// Removes expired challenges. Returns how many were removed.
pub async fn prune_challenges(db: impl PgExecutor<'_>) -> sqlx::Result<u64> {
    let result = sqlx::query("DELETE FROM login_challenges WHERE expires_at < now()")
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use moekura_core::totp::hash_recovery_code;
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
    async fn enrolment_steps_and_recovery_codes(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        assert!(get(&pool, alice).await.unwrap().is_none());
        let first = Secret::generate();
        begin(&pool, alice, &first).await.unwrap();
        let second = Secret::generate();
        begin(&pool, alice, &second).await.unwrap();
        let totp = get(&pool, alice).await.unwrap().unwrap();
        assert_eq!(totp.secret, second, "an unfinished setup is replaced");
        assert!(!totp.enabled);
        assert!(!is_enabled(&pool, alice).await.unwrap());
        assert!(!use_step(&pool, alice, 5).await.unwrap(), "not on yet");

        let codes = [
            hash_recovery_code("aaaaa-bbbbb"),
            hash_recovery_code("ccccc-ddddd"),
        ];
        let mut conn = pool.acquire().await.unwrap();
        enable(&mut conn, alice, 10, &codes).await.unwrap();
        assert!(is_enabled(&pool, alice).await.unwrap());
        // Once on, starting over doesn't replace the secret.
        begin(&pool, alice, &Secret::generate()).await.unwrap();
        assert_eq!(get(&pool, alice).await.unwrap().unwrap().secret, second);

        assert!(!use_step(&pool, alice, 10).await.unwrap(), "already used");
        assert!(use_step(&pool, alice, 11).await.unwrap());
        assert!(!use_step(&pool, alice, 11).await.unwrap());

        assert_eq!(unused_recovery_codes(&pool, alice).await.unwrap(), 2);
        let code = hash_recovery_code("AAAAA BBBBB");
        assert!(use_recovery_code(&pool, alice, &code).await.unwrap());
        assert!(!use_recovery_code(&pool, alice, &code).await.unwrap());
        assert_eq!(unused_recovery_codes(&pool, alice).await.unwrap(), 1);

        assert!(disable(&mut conn, alice).await.unwrap());
        assert!(!disable(&mut conn, alice).await.unwrap());
        assert_eq!(unused_recovery_codes(&pool, alice).await.unwrap(), 0);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn challenges_count_attempts_and_expire(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let token = challenge(&pool, alice, Some("/posts"), Duration::from_secs(300))
            .await
            .unwrap();
        assert_eq!(pending(&pool, &token).await.unwrap().unwrap().attempts, 0);
        let first = attempt(&pool, &token).await.unwrap().unwrap();
        assert_eq!(
            first,
            Challenge {
                user_id: alice,
                next: Some("/posts".into()),
                attempts: 1
            }
        );
        assert_eq!(attempt(&pool, &token).await.unwrap().unwrap().attempts, 2);
        end_challenge(&pool, &token).await.unwrap();
        assert!(attempt(&pool, &token).await.unwrap().is_none());

        let expired = challenge(&pool, alice, None, Duration::ZERO).await.unwrap();
        assert!(pending(&pool, &expired).await.unwrap().is_none());
        assert_eq!(prune_challenges(&pool).await.unwrap(), 1);
    }
}
