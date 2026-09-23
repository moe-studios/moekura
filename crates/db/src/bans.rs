//! User bans and IP bans.

use std::net::IpAddr;

use ipnet::IpNet;
use sqlx::PgExecutor;
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

pub async fn ban(
    db: impl PgExecutor<'_>,
    user_id: i64,
    reason: &str,
    expires_at: Option<OffsetDateTime>,
    banner_id: Option<i64>,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO bans (user_id, reason, expires_at, banner_id) VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(user_id)
    .bind(reason)
    .bind(expires_at)
    .bind(banner_id)
    .fetch_one(db)
    .await
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

pub async fn lift_network(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<IpNet>> {
    sqlx::query_scalar(
        "UPDATE ip_bans SET lifted_at = now() WHERE id = $1 AND lifted_at IS NULL RETURNING network",
    )
    .bind(id)
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
        let past = OffsetDateTime::now_utc() - time::Duration::days(1);
        ban(&pool, alice, "old", Some(past), None).await.unwrap();
        assert!(active(&pool, 10).await.unwrap().is_empty(), "expired");
        ban(&pool, alice, "spam", None, None).await.unwrap();
        let bans = for_user(&pool, alice).await.unwrap();
        assert_eq!(
            bans.iter()
                .map(|b| (b.reason.as_str(), b.active))
                .collect::<Vec<_>>(),
            [("spam", true), ("old", false)]
        );
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
        assert!(lift_network(&pool, id).await.unwrap().is_some());
        assert!(
            network_ban(&pool, "203.0.113.200".parse().unwrap())
                .await
                .unwrap()
                .is_none()
        );
    }
}
