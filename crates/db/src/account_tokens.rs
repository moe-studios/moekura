//! Single-use links sent by email: confirming an address, resetting a
//! password (`account_tokens`).

use std::time::Duration;

use moekura_core::tokens::{NewToken, hash_token};
use sqlx::{PgConnection, PgExecutor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Confirms `email` and makes it the account's address.
    VerifyEmail,
    ResetPassword,
}

impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Purpose::VerifyEmail => "verify_email",
            Purpose::ResetPassword => "reset_password",
        }
    }
}

/// What a valid token was issued for.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Redeemed {
    pub user_id: i64,
    pub email: String,
}

/// A new token for `user_id` to use within `ttl`, replacing any earlier
/// one for the same purpose. Returns the token for the link.
pub async fn issue(
    conn: &mut PgConnection,
    user_id: i64,
    purpose: Purpose,
    email: &str,
    ttl: Duration,
) -> sqlx::Result<String> {
    // Tidy up while here: expired tokens are useless to everyone.
    sqlx::query(
        "DELETE FROM account_tokens
         WHERE (user_id = $1 AND purpose = $2) OR expires_at < now()",
    )
    .bind(user_id)
    .bind(purpose.as_str())
    .execute(&mut *conn)
    .await?;
    let token = NewToken::generate();
    sqlx::query(
        "INSERT INTO account_tokens (user_id, purpose, token_hash, email, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))",
    )
    .bind(user_id)
    .bind(purpose.as_str())
    .bind(&token.hash[..])
    .bind(email)
    .bind(ttl.as_secs_f64())
    .execute(&mut *conn)
    .await?;
    Ok(token.token)
}

/// What `token` is for, if it's valid, without using it up (to show a
/// form that will).
pub async fn peek(
    db: impl PgExecutor<'_>,
    purpose: Purpose,
    token: &str,
) -> sqlx::Result<Option<Redeemed>> {
    sqlx::query_as(
        "SELECT user_id, email::text FROM account_tokens
         WHERE token_hash = $1 AND purpose = $2 AND expires_at > now()",
    )
    .bind(&hash_token(token)[..])
    .bind(purpose.as_str())
    .fetch_optional(db)
    .await
}

/// Uses up `token`: what it was for, if it was valid.
pub async fn redeem(
    db: impl PgExecutor<'_>,
    purpose: Purpose,
    token: &str,
) -> sqlx::Result<Option<Redeemed>> {
    sqlx::query_as(
        "DELETE FROM account_tokens
         WHERE token_hash = $1 AND purpose = $2 AND expires_at > now()
         RETURNING user_id, email::text",
    )
    .bind(&hash_token(token)[..])
    .bind(purpose.as_str())
    .fetch_optional(db)
    .await
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
    async fn tokens_are_single_use_and_replace_each_other(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let hour = Duration::from_secs(3600);
        let mut conn = pool.acquire().await.unwrap();
        let first = issue(
            &mut conn,
            alice,
            Purpose::VerifyEmail,
            "a@example.com",
            hour,
        )
        .await
        .unwrap();
        let second = issue(
            &mut conn,
            alice,
            Purpose::VerifyEmail,
            "b@example.com",
            hour,
        )
        .await
        .unwrap();
        let reset = issue(
            &mut conn,
            alice,
            Purpose::ResetPassword,
            "b@example.com",
            hour,
        )
        .await
        .unwrap();

        // Only the latest of each purpose works, and only for its purpose.
        assert!(
            redeem(&pool, Purpose::VerifyEmail, &first)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            redeem(&pool, Purpose::VerifyEmail, &reset)
                .await
                .unwrap()
                .is_none()
        );
        let expected = Redeemed {
            user_id: alice,
            email: "b@example.com".into(),
        };
        assert_eq!(
            peek(&pool, Purpose::VerifyEmail, &second).await.unwrap(),
            Some(expected.clone())
        );
        assert_eq!(
            redeem(&pool, Purpose::VerifyEmail, &second).await.unwrap(),
            Some(expected)
        );
        assert!(
            redeem(&pool, Purpose::VerifyEmail, &second)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            peek(&pool, Purpose::ResetPassword, &reset)
                .await
                .unwrap()
                .is_some()
        );

        let expired = issue(
            &mut conn,
            alice,
            Purpose::ResetPassword,
            "b@example.com",
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert!(
            redeem(&pool, Purpose::ResetPassword, &expired)
                .await
                .unwrap()
                .is_none()
        );
    }
}
