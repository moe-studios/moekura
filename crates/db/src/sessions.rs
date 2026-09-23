//! Queries on `sessions`.
//!
//! Expiry slides: each use pushes `expires_at` out to `idle` from now,
//! capped at `max` after login. To avoid a write on every request, a
//! session is only touched when its last recorded use is older than
//! [`TOUCH_INTERVAL`].

use std::net::IpAddr;
use std::time::Duration;

use sqlx::PgExecutor;
use time::OffsetDateTime;
use uwuu_core::tokens::{NewToken, hash_token};

use crate::bans::ActiveBan;
use crate::users::User;

pub const TOUCH_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy)]
pub struct Lifetime {
    pub idle: Duration,
    pub max: Duration,
}

pub struct NewSession<'a> {
    pub user_id: i64,
    pub user_agent: Option<&'a str>,
    pub ip: Option<IpAddr>,
}

/// A live session and its (active) user.
#[derive(Debug, Clone)]
pub struct SessionUser {
    pub session_id: i64,
    pub last_used_at: OffsetDateTime,
    pub user: User,
    /// The user's ban in force, if any.
    pub ban: Option<ActiveBan>,
}

impl SessionUser {
    pub fn needs_touch(&self, now: OffsetDateTime) -> bool {
        now - self.last_used_at >= TOUCH_INTERVAL
    }
}

/// Creates a session and returns the token for the client's cookie.
pub async fn create(
    db: impl PgExecutor<'_>,
    session: NewSession<'_>,
    lifetime: Lifetime,
) -> sqlx::Result<String> {
    let NewToken { token, hash } = NewToken::generate();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, expires_at, user_agent, ip)
         VALUES ($1, $2, now() + make_interval(secs => $3), $4, $5)",
    )
    .bind(&hash[..])
    .bind(session.user_id)
    .bind(lifetime.idle.as_secs_f64())
    .bind(session.user_agent.map(|ua| truncate(ua, 512)))
    .bind(session.ip)
    .execute(db)
    .await?;
    Ok(token)
}

/// The session for `token` if it is unexpired and its user is active.
pub async fn lookup(db: impl PgExecutor<'_>, token: &str) -> sqlx::Result<Option<SessionUser>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        session_id: i64,
        last_used_at: OffsetDateTime,
        #[sqlx(flatten)]
        user: User,
        banned: bool,
        ban_reason: Option<String>,
        ban_expires_at: Option<OffsetDateTime>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT s.id AS session_id, s.last_used_at,
                u.id, u.name::text, u.email::text, u.role_id, u.status, u.created_at, u.last_seen_at, u.settings,
                b.id IS NOT NULL AS banned, b.reason AS ban_reason, b.expires_at AS ban_expires_at
         FROM sessions s JOIN users u ON u.id = s.user_id
         LEFT JOIN LATERAL (
             SELECT id, reason, expires_at FROM bans
             WHERE user_id = u.id AND lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
             ORDER BY expires_at DESC NULLS FIRST LIMIT 1
         ) b ON true
         WHERE s.token_hash = $1 AND s.expires_at > now() AND u.status = 'active'",
    )
    .bind(&hash_token(token)[..])
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| SessionUser {
        session_id: r.session_id,
        last_used_at: r.last_used_at,
        user: r.user,
        ban: r.banned.then(|| ActiveBan {
            reason: r.ban_reason.unwrap_or_default(),
            expires_at: r.ban_expires_at,
        }),
    }))
}

/// Records use of a session: slides its expiry and updates the user's
/// last-seen time.
pub async fn touch(
    db: impl PgExecutor<'_>,
    session_id: i64,
    lifetime: Lifetime,
) -> sqlx::Result<()> {
    sqlx::query(
        "WITH s AS (
             UPDATE sessions
             SET last_used_at = now(),
                 expires_at = least(now() + make_interval(secs => $2),
                                    created_at + make_interval(secs => $3))
             WHERE id = $1
             RETURNING user_id
         )
         UPDATE users SET last_seen_at = now() FROM s WHERE users.id = s.user_id",
    )
    .bind(session_id)
    .bind(lifetime.idle.as_secs_f64())
    .bind(lifetime.max.as_secs_f64())
    .execute(db)
    .await?;
    Ok(())
}

pub async fn delete(db: impl PgExecutor<'_>, token: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
        .bind(&hash_token(token)[..])
        .execute(db)
        .await?;
    Ok(())
}

/// Logs a user out everywhere. Returns how many sessions ended.
pub async fn delete_all_for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<u64> {
    let result = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

/// Removes expired sessions. Returns how many were removed.
pub async fn prune_expired(db: impl PgExecutor<'_>) -> sqlx::Result<u64> {
    let result = sqlx::query("DELETE FROM sessions WHERE expires_at <= now()")
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

fn truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uwuu_core::permissions::SystemRole;

    use super::*;
    use crate::roles;
    use crate::users::{self, NewUser, UserStatus};

    const LIFETIME: Lifetime = Lifetime {
        idle: Duration::from_secs(30 * 86_400),
        max: Duration::from_secs(365 * 86_400),
    };

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

    async fn start(db: &PgPool, user_id: i64) -> String {
        let ip = Some("203.0.113.9".parse().unwrap());
        create(
            db,
            NewSession {
                user_id,
                user_agent: Some("test"),
                ip,
            },
            LIFETIME,
        )
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn create_and_lookup(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let token = start(&pool, alice.id).await;
        let found = lookup(&pool, &token).await.unwrap().unwrap();
        assert_eq!(found.user.id, alice.id);
        assert!(lookup(&pool, "not-a-token").await.unwrap().is_none());

        // Only the hash is stored.
        let stored: Vec<u8> = sqlx::query_scalar("SELECT token_hash FROM sessions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(stored, token.as_bytes());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn expired_sessions_and_inactive_users_are_ignored(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let token = start(&pool, alice.id).await;

        users::set_status(&pool, alice.id, UserStatus::Deactivated)
            .await
            .unwrap();
        assert!(lookup(&pool, &token).await.unwrap().is_none());
        users::set_status(&pool, alice.id, UserStatus::Active)
            .await
            .unwrap();

        sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(lookup(&pool, &token).await.unwrap().is_none());
        assert_eq!(prune_expired(&pool).await.unwrap(), 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn touch_slides_expiry_up_to_the_cap(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let token = start(&pool, alice.id).await;
        // Pretend the session is old and nearly at its absolute limit.
        sqlx::query(
            "UPDATE sessions SET created_at = now() - interval '364 days',
                                 last_used_at = now() - interval '2 hours'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let found = lookup(&pool, &token).await.unwrap().unwrap();
        assert!(found.needs_touch(OffsetDateTime::now_utc()));
        touch(&pool, found.session_id, LIFETIME).await.unwrap();

        let (days_left, seen): (f64, Option<OffsetDateTime>) = sqlx::query_as(
            "SELECT extract(epoch FROM s.expires_at - now())::float8 / 86400, u.last_seen_at
             FROM sessions s JOIN users u ON u.id = s.user_id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            (0.9..1.1).contains(&days_left),
            "capped at created_at + max, got {days_left}"
        );
        assert!(seen.is_some());
        let fresh = lookup(&pool, &token).await.unwrap().unwrap();
        assert!(!fresh.needs_touch(OffsetDateTime::now_utc()));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn logout_and_logout_everywhere(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let a1 = start(&pool, alice.id).await;
        let a2 = start(&pool, alice.id).await;
        let b1 = start(&pool, bob.id).await;

        delete(&pool, &a1).await.unwrap();
        assert!(lookup(&pool, &a1).await.unwrap().is_none());
        assert!(lookup(&pool, &a2).await.unwrap().is_some());

        assert_eq!(delete_all_for_user(&pool, alice.id).await.unwrap(), 1);
        assert!(lookup(&pool, &a2).await.unwrap().is_none());
        assert!(lookup(&pool, &b1).await.unwrap().is_some());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("héllo", 2), "h");
        assert_eq!(truncate("short", 512), "short");
    }
}
