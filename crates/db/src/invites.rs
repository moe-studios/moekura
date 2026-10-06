//! Queries on `invites`.

use std::time::Duration;

use moekura_core::tokens::{NewToken, hash_token};
use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

pub struct NewInvite {
    pub created_by: Option<i64>,
    pub max_uses: i32,
    /// `None` never expires.
    pub expires_in: Option<Duration>,
    /// Who it's for, say; seen by those who can see the invite.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Invite {
    pub id: i64,
    pub created_by: Option<i64>,
    pub creator_name: Option<String>,
    pub note: String,
    pub max_uses: i32,
    pub uses: i32,
    pub expires_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    /// Names of the accounts made with it, first first.
    pub used_by: Vec<String>,
}

impl Invite {
    /// Whether it can still be used.
    pub fn is_usable(&self) -> bool {
        self.revoked_at.is_none()
            && self.uses < self.max_uses
            && self
                .expires_at
                .is_none_or(|at| at > OffsetDateTime::now_utc())
    }
}

/// `SELECT <invite columns> FROM invites i …` followed by `$rest`.
macro_rules! select_invites {
    ($rest:literal) => {
        concat!(
            "SELECT i.id, i.created_by, c.name::text AS creator_name, i.note, i.max_uses,
                    i.uses, i.expires_at, i.revoked_at, i.created_at,
                    ARRAY(SELECT u.name::text FROM users u WHERE u.invite_id = i.id
                          ORDER BY u.id) AS used_by
             FROM invites i LEFT JOIN users c ON c.id = i.created_by ",
            $rest
        )
    };
}

/// Creates an invite and returns its code, which is shown once.
pub async fn create(db: impl PgExecutor<'_>, invite: NewInvite) -> sqlx::Result<String> {
    let NewToken { token, hash } = NewToken::generate();
    sqlx::query(
        "INSERT INTO invites (code_hash, created_by, max_uses, expires_at, note)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4), $5)",
    )
    .bind(&hash[..])
    .bind(invite.created_by)
    .bind(invite.max_uses)
    .bind(invite.expires_in.map(|d| d.as_secs_f64()))
    .bind(&invite.note)
    .execute(db)
    .await?;
    Ok(token)
}

/// Creates an invite, as [`create`], unless its creator already made
/// `quota` in the last `days` days; `None` then. The creator's row is
/// locked while counting, so requests sent together can't each find room
/// under the quota.
pub async fn create_within_quota(
    db: &PgPool,
    invite: NewInvite,
    quota: i64,
    days: i64,
) -> sqlx::Result<Option<String>> {
    let mut tx = db.begin().await?;
    if let Some(creator) = invite.created_by {
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
            .bind(creator)
            .execute(&mut *tx)
            .await?;
        if made_since(&mut *tx, creator, days).await? >= quota {
            return Ok(None);
        }
    }
    let code = create(&mut *tx, invite).await?;
    tx.commit().await?;
    Ok(Some(code))
}

/// Uses up one redemption of `code`, returning the invite's id, or `None`
/// when the code is unknown, expired, revoked or used up. Run it in the
/// same transaction as the account creation, then [`record_use`], so a
/// failed signup doesn't consume the invite.
pub async fn redeem(db: impl PgExecutor<'_>, code: &str) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "UPDATE invites SET uses = uses + 1
         WHERE code_hash = $1 AND uses < max_uses AND revoked_at IS NULL
           AND (expires_at IS NULL OR expires_at > now())
         RETURNING id",
    )
    .bind(&hash_token(code.trim())[..])
    .fetch_optional(db)
    .await
}

/// Notes that `user_id` signed up with invite `invite_id`.
pub async fn record_use(db: impl PgExecutor<'_>, invite_id: i64, user_id: i64) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET invite_id = $1 WHERE id = $2")
        .bind(invite_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Invites, newest first: everyone's, or `created_by`'s.
pub async fn list(
    db: impl PgExecutor<'_>,
    created_by: Option<i64>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Invite>> {
    sqlx::query_as(select_invites!(
        "WHERE ($1::bigint IS NULL OR i.created_by = $1)
         ORDER BY i.id DESC LIMIT $2 OFFSET $3"
    ))
    .bind(created_by)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Invite>> {
    sqlx::query_as(select_invites!("WHERE i.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Stops an invite working. False when it already was revoked.
pub async fn revoke(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<bool> {
    let result =
        sqlx::query("UPDATE invites SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .execute(db)
            .await?;
    Ok(result.rows_affected() == 1)
}

/// How many invites `user_id` made in the last `days` days.
pub async fn made_since(db: impl PgExecutor<'_>, user_id: i64, days: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM invites
         WHERE created_by = $1 AND created_at > now() - make_interval(days => $2::int)",
    )
    .bind(user_id)
    .bind(days)
    .fetch_one(db)
    .await
}

/// The id and name of whoever made the invite `user_id` signed up with.
pub async fn inviter(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Option<(i64, String)>> {
    sqlx::query_as(
        "SELECT c.id, c.name::text FROM users u
         JOIN invites i ON i.id = u.invite_id JOIN users c ON c.id = i.created_by
         WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
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
            note: String::new(),
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn codes_redeem_up_to_their_limit(pool: PgPool) {
        let code = create(&pool, invite(2, None)).await.unwrap();
        assert!(redeem(&pool, &code).await.unwrap().is_some());
        // Surrounding whitespace from copy-pasting is ignored.
        assert!(
            redeem(&pool, &format!(" {code}\n"))
                .await
                .unwrap()
                .is_some()
        );
        assert!(redeem(&pool, &code).await.unwrap().is_none());
        assert!(redeem(&pool, "made-up").await.unwrap().is_none());
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
        assert!(redeem(&pool, &code).await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn revoked_codes_do_not_redeem(pool: PgPool) {
        let code = create(&pool, invite(3, None)).await.unwrap();
        let id = redeem(&pool, &code).await.unwrap().unwrap();
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        record_use(&pool, id, user).await.unwrap();
        assert!(revoke(&pool, id).await.unwrap());
        assert!(!revoke(&pool, id).await.unwrap());
        assert!(redeem(&pool, &code).await.unwrap().is_none());
        let found = by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(found.used_by, ["alice"]);
        assert!(!found.is_usable());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn quotas_hold_against_requests_sent_together(pool: PgPool) {
        let alice: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mine = || NewInvite {
            created_by: Some(alice),
            ..invite(1, None)
        };
        create(&pool, mine()).await.unwrap();
        // One request has counted and is making its invite, holding the
        // inviter's row …
        let mut first = pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
            .bind(alice)
            .execute(&mut *first)
            .await
            .unwrap();
        assert_eq!(made_since(&mut *first, alice, 30).await.unwrap(), 1);
        // … when another comes: it waits, then counts the first's invite.
        let second = create_within_quota(&pool, mine(), 2, 30);
        let finish_first = async {
            waiting_on_lock(&pool).await;
            create(&mut *first, mine()).await.unwrap();
            first.commit().await.unwrap();
        };
        let (second, ()) = tokio::join!(second, finish_first);
        assert_eq!(second.unwrap(), None);
        assert_eq!(made_since(&pool, alice, 30).await.unwrap(), 2);
        // Older invites don't count.
        sqlx::query("UPDATE invites SET created_at = now() - interval '31 days'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            create_within_quota(&pool, mine(), 2, 30)
                .await
                .unwrap()
                .is_some()
        );
    }

    /// Returns once a query in this test's database waits for a lock, or
    /// after a few seconds.
    async fn waiting_on_lock(pool: &PgPool) {
        for _ in 0..300 {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                                WHERE datname = current_database() AND wait_event_type = 'Lock')",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn rolled_back_redemptions_are_not_counted(pool: PgPool) {
        let code = create(&pool, invite(1, None)).await.unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(redeem(&mut *tx, &code).await.unwrap().is_some());
        tx.rollback().await.unwrap();
        assert!(redeem(&pool, &code).await.unwrap().is_some());
    }
}
