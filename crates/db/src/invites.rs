//! Queries on `invites`.

use std::time::Duration;

use sqlx::PgExecutor;
use uwuu_core::tokens::{NewToken, hash_token};

pub struct NewInvite {
    pub created_by: Option<i64>,
    pub max_uses: i32,
    /// `None` never expires.
    pub expires_in: Option<Duration>,
}

/// Creates an invite and returns its code, which is shown once.
pub async fn create(db: impl PgExecutor<'_>, invite: NewInvite) -> sqlx::Result<String> {
    let NewToken { token, hash } = NewToken::generate();
    sqlx::query(
        "INSERT INTO invites (code_hash, created_by, max_uses, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
    )
    .bind(&hash[..])
    .bind(invite.created_by)
    .bind(invite.max_uses)
    .bind(invite.expires_in.map(|d| d.as_secs_f64()))
    .execute(db)
    .await?;
    Ok(token)
}

/// Uses up one redemption of `code`. Returns false when the code is
/// unknown, expired or used up. Run it in the same transaction as the
/// account creation so a failed signup doesn't consume the invite.
pub async fn redeem(db: impl PgExecutor<'_>, code: &str) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE invites SET uses = uses + 1
         WHERE code_hash = $1 AND uses < max_uses AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(&hash_token(code.trim())[..])
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    fn invite(max_uses: i32, expires_in: Option<Duration>) -> NewInvite {
        NewInvite {
            created_by: None,
            max_uses,
            expires_in,
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn codes_redeem_up_to_their_limit(pool: PgPool) {
        let code = create(&pool, invite(2, None)).await.unwrap();
        assert!(redeem(&pool, &code).await.unwrap());
        // Surrounding whitespace from copy-pasting is ignored.
        assert!(redeem(&pool, &format!(" {code}\n")).await.unwrap());
        assert!(!redeem(&pool, &code).await.unwrap());
        assert!(!redeem(&pool, "made-up").await.unwrap());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn expired_codes_do_not_redeem(pool: PgPool) {
        let code = create(&pool, invite(1, Some(Duration::from_secs(3600))))
            .await
            .unwrap();
        sqlx::query("UPDATE invites SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(!redeem(&pool, &code).await.unwrap());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn rolled_back_redemptions_are_not_counted(pool: PgPool) {
        let code = create(&pool, invite(1, None)).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(redeem(&mut *tx, &code).await.unwrap());
        tx.rollback().await.unwrap();
        assert!(redeem(&pool, &code).await.unwrap());
    }
}
