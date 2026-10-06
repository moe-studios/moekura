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
    /// The network can't see the site at all, rather than only not
    /// change anything.
    pub full: bool,
}

/// Tells every node's site cache that network bans changed; call it in
/// the transaction making the change.
async fn announce(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_notify($1, 'ip_bans')")
        .bind(crate::site_cache::CHANNEL)
        .execute(conn)
        .await?;
    Ok(())
}

/// Bans a network: fully (it can't see the site) or partly (it can't
/// register, log in or change anything).
pub async fn ban_network(
    conn: &mut PgConnection,
    network: IpNet,
    reason: &str,
    expires_at: Option<OffsetDateTime>,
    full: bool,
    banner_id: Option<i64>,
) -> sqlx::Result<i64> {
    let id = sqlx::query_scalar(
        "INSERT INTO ip_bans (network, reason, expires_at, full_ban, banner_id)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(network.trunc())
    .bind(reason)
    .bind(expires_at)
    .bind(full)
    .bind(banner_id)
    .fetch_one(&mut *conn)
    .await?;
    announce(conn).await?;
    Ok(id)
}

/// Lifts network ban `id`, returning its network; `None` if it was lifted
/// already.
pub async fn lift_network(
    conn: &mut PgConnection,
    id: i64,
    lifter_id: Option<i64>,
) -> sqlx::Result<Option<IpNet>> {
    let network = sqlx::query_scalar(
        "UPDATE ip_bans SET lifted_at = now(), lifter_id = $2
         WHERE id = $1 AND lifted_at IS NULL RETURNING network",
    )
    .bind(id)
    .bind(lifter_id)
    .fetch_optional(&mut *conn)
    .await?;
    announce(conn).await?;
    Ok(network)
}

/// The network of ban `id`, locked until the transaction ends; `None` if
/// it was lifted.
pub async fn lock_network(conn: &mut PgConnection, id: i64) -> sqlx::Result<Option<IpNet>> {
    sqlx::query_scalar("SELECT network FROM ip_bans WHERE id = $1 AND lifted_at IS NULL FOR UPDATE")
        .bind(id)
        .fetch_optional(conn)
        .await
}

/// Lifts every network ban in force that covers `network` or lies within
/// it, returning their networks.
pub async fn lift_networks_overlapping(
    conn: &mut PgConnection,
    network: IpNet,
    lifter_id: Option<i64>,
) -> sqlx::Result<Vec<IpNet>> {
    let lifted: Vec<IpNet> = sqlx::query_scalar(
        "UPDATE ip_bans SET lifted_at = now(), lifter_id = $2
         WHERE network && $1 AND lifted_at IS NULL
           AND (expires_at IS NULL OR expires_at > now())
         RETURNING network",
    )
    .bind(network.trunc())
    .bind(lifter_id)
    .fetch_all(&mut *conn)
    .await?;
    if !lifted.is_empty() {
        announce(conn).await?;
    }
    Ok(lifted)
}

/// A network ban in force, as the site cache keeps them.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct NetworkBan {
    pub network: IpNet,
    pub reason: String,
    /// `None` until lifted.
    pub expires_at: Option<OffsetDateTime>,
    pub full: bool,
}

impl NetworkBan {
    /// Whether it covers `ip` at `now`.
    pub fn covers(&self, ip: IpAddr, now: OffsetDateTime) -> bool {
        self.network.contains(&ip) && self.expires_at.is_none_or(|t| t > now)
    }
}

/// The network bans in force.
pub async fn networks_in_force(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<NetworkBan>> {
    sqlx::query_as(
        "SELECT network, reason, expires_at, full_ban AS full FROM ip_bans
         WHERE lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
         ORDER BY id DESC",
    )
    .fetch_all(db)
    .await
}

/// IP bans in force, newest first.
pub async fn active_networks(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<IpBan>> {
    sqlx::query_as(
        "SELECT i.id, i.network, i.reason, x.name::text AS banner_name, i.expires_at, i.created_at,
                i.full_ban AS full
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
        let mut conn = pool.acquire().await.unwrap();
        let range: IpNet = "203.0.113.7/24".parse().unwrap();
        let id = ban_network(&mut conn, range, "abuse", None, false, None)
            .await
            .unwrap();
        let week = OffsetDateTime::now_utc() + time::Duration::days(7);
        let v6: IpNet = "2001:db8::/64".parse().unwrap();
        ban_network(&mut conn, v6, "worse", Some(week), true, None)
            .await
            .unwrap();
        let in_force = networks_in_force(&pool).await.unwrap();
        let now = OffsetDateTime::now_utc();
        let covering = |ip: &str| {
            let ip: IpAddr = ip.parse().unwrap();
            in_force
                .iter()
                .find(|b| b.covers(ip, now))
                .map(|b| (b.reason.as_str(), b.full))
        };
        assert_eq!(covering("203.0.113.200"), Some(("abuse", false)));
        assert_eq!(covering("203.0.114.1"), None);
        assert_eq!(covering("2001:db8::1:2"), Some(("worse", true)));
        assert!(!in_force[0].covers("2001:db8::1".parse().unwrap(), week));
        // Stored normalised.
        assert_eq!(
            active_networks(&pool).await.unwrap()[1].network.to_string(),
            "203.0.113.0/24"
        );
        let lifter = user(&pool, "mod").await;
        assert!(
            lift_network(&mut conn, id, Some(lifter))
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
        assert!(lift_network(&mut conn, id, None).await.unwrap().is_none());
        assert!(lock_network(&mut conn, id).await.unwrap().is_none());
        assert_eq!(networks_in_force(&pool).await.unwrap().len(), 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn lifting_the_bans_on_a_range(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        for range in [
            "203.0.0.0/16",
            "203.0.113.0/24",
            "203.0.113.7/32",
            "198.51.100.0/24",
        ] {
            ban_network(&mut conn, range.parse().unwrap(), "x", None, true, None)
                .await
                .unwrap();
        }
        let id = ban_network(
            &mut conn,
            "2001:db8::/64".parse().unwrap(),
            "x",
            None,
            true,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            lock_network(&mut conn, id).await.unwrap(),
            Some("2001:db8::/64".parse().unwrap())
        );
        // Those covering the address, and those within the range.
        let mut lifted =
            lift_networks_overlapping(&mut conn, "203.0.113.0/24".parse().unwrap(), None)
                .await
                .unwrap();
        lifted.sort();
        assert_eq!(
            lifted.iter().map(ToString::to_string).collect::<Vec<_>>(),
            ["203.0.0.0/16", "203.0.113.0/24", "203.0.113.7/32"]
        );
        assert!(
            lift_networks_overlapping(&mut conn, "203.0.113.9/32".parse().unwrap(), None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(networks_in_force(&pool).await.unwrap().len(), 2);
    }
}
