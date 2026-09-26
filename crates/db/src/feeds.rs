//! Feed tokens: reading feeds as a user without a session.

use moekura_core::tokens::{TokenHash, hash_token};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::bans::ActiveBan;
use crate::users::User;

/// Sets (or with `None`, revokes) a user's feed token.
pub async fn set_token(
    db: impl PgExecutor<'_>,
    user_id: i64,
    hash: Option<&TokenHash>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET feed_token_hash = $2 WHERE id = $1")
        .bind(user_id)
        .bind(hash.map(|h| &h[..]))
        .execute(db)
        .await?;
    Ok(())
}

/// Whether a user has a feed token.
pub async fn has_token(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT feed_token_hash IS NOT NULL FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// The active user a feed token belongs to, and their ban if any.
pub async fn user(
    db: impl PgExecutor<'_>,
    token: &str,
) -> sqlx::Result<Option<(User, Option<ActiveBan>)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        user: User,
        banned: bool,
        ban_reason: Option<String>,
        ban_expires_at: Option<OffsetDateTime>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT u.id, u.name::text, u.email::text, u.email_verified_at, u.role_id, u.status,
                u.created_at, u.last_seen_at, u.settings,
                b.id IS NOT NULL AS banned, b.reason AS ban_reason, b.expires_at AS ban_expires_at
         FROM users u
         LEFT JOIN LATERAL (
             SELECT id, reason, expires_at FROM bans
             WHERE user_id = u.id AND lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
             ORDER BY expires_at DESC NULLS FIRST LIMIT 1
         ) b ON true
         WHERE u.feed_token_hash = $1 AND u.status = 'active'",
    )
    .bind(&hash_token(token)[..])
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| {
        let ban = r.banned.then(|| ActiveBan {
            reason: r.ban_reason.unwrap_or_default(),
            expires_at: r.ban_expires_at,
        });
        (r.user, ban)
    }))
}

#[cfg(test)]
mod tests {
    use moekura_core::tokens::NewToken;
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn tokens(pool: PgPool) {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let token = NewToken::generate();
        assert!(!has_token(&pool, id).await.unwrap());
        set_token(&pool, id, Some(&token.hash)).await.unwrap();
        assert!(has_token(&pool, id).await.unwrap());
        let (found, ban) = user(&pool, &token.token).await.unwrap().unwrap();
        assert_eq!((found.name.as_str(), ban), ("alice", None));
        assert!(user(&pool, "nope").await.unwrap().is_none());
        set_token(&pool, id, None).await.unwrap();
        assert!(user(&pool, &token.token).await.unwrap().is_none());
    }
}
