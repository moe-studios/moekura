//! User bans and IP bans.

use std::net::IpAddr;

use ipnet::IpNet;
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

/// A ban in force.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ActiveBan {
    pub reason: String,
    /// `None` until lifted.
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Ban {
    pub id: i64,
    pub user_id: i64,
    pub user_name: String,
    pub reason: String,
    pub banner_name: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
    pub lifted_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub active: bool,
}

/// `SELECT <ban columns> …` followed by `$rest`.
macro_rules! select_bans {
    ($rest:literal) => {
        concat!(
            "SELECT b.id, b.user_id, u.name::text AS user_name, b.reason,
                    x.name::text AS banner_name, b.expires_at, b.lifted_at, b.created_at,
                    (b.lifted_at IS NULL AND (b.expires_at IS NULL OR b.expires_at > now())) AS active
             FROM bans b JOIN users u ON u.id = b.user_id
             LEFT JOIN users x ON x.id = b.banner_id ",
            $rest
        )
    };
}

/// Bans a user, replacing any ban in force (it's lifted by `banner_id`).
/// Returns the ban it replaced. The user is locked, so bans made at the
/// same moment don't both stand.
pub async fn ban(
    conn: &mut PgConnection,
    user_id: i64,
    reason: &str,
    expires_at: Option<OffsetDateTime>,
    banner_id: Option<i64>,
) -> sqlx::Result<Option<ActiveBan>> {
    sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .execute(&mut *conn)
        .await?;
    // Newest last; before bans were replaced, several could be in force.
    let replaced: Vec<ActiveBan> = sqlx::query_as(
        "WITH lifted AS (
             UPDATE bans SET lifted_at = now(), lifter_id = $2
             WHERE user_id = $1 AND lifted_at IS NULL
               AND (expires_at IS NULL OR expires_at > now())
             RETURNING id, reason, expires_at)
         SELECT reason, expires_at FROM lifted ORDER BY id",
    )
    .bind(user_id)
    .bind(banner_id)
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO bans (user_id, reason, expires_at, banner_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(reason)
    .bind(expires_at)
    .bind(banner_id)
    .execute(&mut *conn)
    .await?;
    Ok(replaced.into_iter().last())
}

/// Lifts a user's active bans; returns how many.
pub async fn lift(
    db: impl PgExecutor<'_>,
    user_id: i64,
    lifter_id: Option<i64>,
) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE bans SET lifted_at = now(), lifter_id = $2
         WHERE user_id = $1 AND lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(user_id)
    .bind(lifter_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

/// A user's bans, newest first.
pub async fn for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<Ban>> {
    sqlx::query_as(select_bans!("WHERE b.user_id = $1 ORDER BY b.id DESC"))
        .bind(user_id)
        .fetch_all(db)
        .await
}

/// Which of `user_ids` are under a ban.
pub async fn banned_among(db: impl PgExecutor<'_>, user_ids: &[i64]) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT DISTINCT user_id FROM bans
         WHERE user_id = ANY($1) AND lifted_at IS NULL
           AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(user_ids)
    .fetch_all(db)
    .await
}

/// Bans in force, newest first.
pub async fn active(db: impl PgExecutor<'_>, limit: i64) -> sqlx::Result<Vec<Ban>> {
    sqlx::query_as(select_bans!(
        "WHERE b.lifted_at IS NULL AND (b.expires_at IS NULL OR b.expires_at > now())
         ORDER BY b.id DESC LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(db)
    .await
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct IpBan {
    pub id: i64,
    pub network: IpNet,
    pub reason: String,
    pub banner_name: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

pub async fn ban_network(
    db: impl PgExecutor<'_>,
    network: IpNet,
    reason: &str,
    expires_at: Option<OffsetDateTime>,
    banner_id: Option<i64>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO ip_bans (network, reason, expires_at, banner_id) VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(network.trunc())
    .bind(reason)
    .bind(expires_at)
    .bind(banner_id)
    .fetch_one(db)
    .await
}

/// Lifts network ban `id`, returning its network; `None` if it was lifted
/// already.
pub async fn lift_network(
    db: impl PgExecutor<'_>,
    id: i64,
    lifter_id: Option<i64>,
) -> sqlx::Result<Option<IpNet>> {
    sqlx::query_scalar(
        "UPDATE ip_bans SET lifted_at = now(), lifter_id = $2
         WHERE id = $1 AND lifted_at IS NULL RETURNING network",
    )
    .bind(id)
    .bind(lifter_id)
    .fetch_optional(db)
    .await
}

/// The reason `ip` is banned, if it is.
pub async fn network_ban(db: impl PgExecutor<'_>, ip: IpAddr) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT reason FROM ip_bans
         WHERE network >>= $1 AND lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
         ORDER BY id DESC LIMIT 1",
    )
    .bind(IpNet::from(ip))
    .fetch_optional(db)
    .await
}

/// IP bans in force, newest first.
pub async fn active_networks(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<IpBan>> {
    sqlx::query_as(
        "SELECT i.id, i.network, i.reason, x.name::text AS banner_name, i.expires_at, i.created_at
         FROM ip_bans i LEFT JOIN users x ON x.id = i.banner_id
         WHERE i.lifted_at IS NULL AND (i.expires_at IS NULL OR i.expires_at > now())
         ORDER BY i.id DESC LIMIT 500",
    )
    .fetch_all(db)
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
    async fn user_bans(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let mut conn = pool.acquire().await.unwrap();
        let past = OffsetDateTime::now_utc() - time::Duration::days(1);
        ban(&mut conn, alice, "old", Some(past), None)
            .await
            .unwrap();
        assert!(active(&pool, 10).await.unwrap().is_empty(), "expired");
        let replaced = ban(&mut conn, alice, "spam", None, None).await.unwrap();
        assert_eq!(replaced, None, "the expired ban wasn't in force");
        let bans = for_user(&pool, alice).await.unwrap();
        assert_eq!(
            bans.iter()
                .map(|b| (b.reason.as_str(), b.active))
                .collect::<Vec<_>>(),
            [("spam", true), ("old", false)]
        );
        // A new ban replaces the one in force.
        let week = OffsetDateTime::now_utc() + time::Duration::days(7);
        let replaced = ban(&mut conn, alice, "spam, a week", Some(week), None)
            .await
            .unwrap();
        assert_eq!(
            replaced,
            Some(ActiveBan {
                reason: "spam".into(),
                expires_at: None
            })
        );
        let active_now = active(&pool, 10).await.unwrap();
        assert_eq!(active_now.len(), 1);
        assert_eq!(active_now[0].reason, "spam, a week");
        assert_eq!(lift(&pool, alice, None).await.unwrap(), 1);
        assert!(active(&pool, 10).await.unwrap().is_empty());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn network_bans(pool: PgPool) {
        let range: IpNet = "203.0.113.7/24".parse().unwrap();
        let id = ban_network(&pool, range, "abuse", None, None)
            .await
            .unwrap();
        assert_eq!(
            network_ban(&pool, "203.0.113.200".parse().unwrap())
                .await
                .unwrap()
                .as_deref(),
            Some("abuse")
        );
        assert!(
            network_ban(&pool, "203.0.114.1".parse().unwrap())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            network_ban(&pool, "2001:db8::1".parse().unwrap())
                .await
                .unwrap()
                .is_none()
        );
        // Stored normalised.
        assert_eq!(
            active_networks(&pool).await.unwrap()[0].network.to_string(),
            "203.0.113.0/24"
        );
        let lifter = user(&pool, "mod").await;
        assert!(
            lift_network(&pool, id, Some(lifter))
                .await
                .unwrap()
                .is_some()
        );
        let lifted_by: Option<i64> =
            sqlx::query_scalar("SELECT lifter_id FROM ip_bans WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(lifted_by, Some(lifter));
        assert!(lift_network(&pool, id, None).await.unwrap().is_none());
        assert!(
            network_ban(&pool, "203.0.113.200".parse().unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }
}
