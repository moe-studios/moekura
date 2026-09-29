//! Which addresses each account used, for staff.

use std::net::IpAddr;

use ipnet::IpNet;
use sqlx::PgExecutor;
use time::OffsetDateTime;

/// Records that `user_id` used `ip` now. An address seen within the hour
/// isn't written again, so calling this on every change is cheap.
pub async fn record(db: impl PgExecutor<'_>, user_id: i64, ip: IpAddr) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO user_ips (user_id, ip) VALUES ($1, $2)
         ON CONFLICT (user_id, ip) DO UPDATE SET last_seen_at = now()
         WHERE user_ips.last_seen_at < now() - interval '1 hour'",
    )
    .bind(user_id)
    .bind(IpNet::from(ip))
    .execute(db)
    .await?;
    Ok(())
}

/// An address a user used.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Seen {
    pub ip: IpNet,
    pub first_seen_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
}

/// The addresses `user_id` used, most recent first.
pub async fn for_user(
    db: impl PgExecutor<'_>,
    user_id: i64,
    limit: i64,
) -> sqlx::Result<Vec<Seen>> {
    sqlx::query_as(
        "SELECT ip, first_seen_at, last_seen_at FROM user_ips WHERE user_id = $1
         ORDER BY last_seen_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Another account seen on one of a user's addresses.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Related {
    pub name: String,
    pub ip: IpNet,
    pub last_seen_at: OffsetDateTime,
}

/// Other accounts seen on the addresses `user_id` used, most recently
/// seen first.
pub async fn related(
    db: impl PgExecutor<'_>,
    user_id: i64,
    limit: i64,
) -> sqlx::Result<Vec<Related>> {
    sqlx::query_as(
        "SELECT u.name::text AS name, o.ip, o.last_seen_at
         FROM user_ips mine
         JOIN user_ips o ON o.ip = mine.ip AND o.user_id <> mine.user_id
         JOIN users u ON u.id = o.user_id
         WHERE mine.user_id = $1
         ORDER BY o.last_seen_at DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Forgets addresses not seen for `days` days; returns how many.
pub async fn prune(db: impl PgExecutor<'_>, days: u32) -> sqlx::Result<u64> {
    let result =
        sqlx::query("DELETE FROM user_ips WHERE last_seen_at < now() - make_interval(days => $1)")
            .bind(i32::try_from(days).unwrap_or(i32::MAX))
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
    async fn records_relates_and_prunes(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let shared: IpAddr = "203.0.113.7".parse().unwrap();
        let own: IpAddr = "2001:db8::1".parse().unwrap();
        record(&pool, alice, shared).await.unwrap();
        record(&pool, alice, own).await.unwrap();
        record(&pool, alice, own).await.unwrap();
        record(&pool, bob, shared).await.unwrap();

        let seen = for_user(&pool, alice, 10).await.unwrap();
        assert_eq!(seen.len(), 2);
        let related = related(&pool, alice, 10).await.unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(
            (related[0].name.as_str(), related[0].ip.addr()),
            ("bob", shared)
        );

        // Addresses unseen for longer than the retention are forgotten.
        sqlx::query(
            "UPDATE user_ips SET last_seen_at = now() - interval '2 days' WHERE user_id = $1",
        )
        .bind(bob)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(prune(&pool, 1).await.unwrap(), 1);
        assert!(for_user(&pool, bob, 10).await.unwrap().is_empty());
        assert_eq!(prune(&pool, 1).await.unwrap(), 0);
        record(&pool, alice, own).await.unwrap();
        assert_eq!(for_user(&pool, alice, 10).await.unwrap().len(), 2);
    }
}
