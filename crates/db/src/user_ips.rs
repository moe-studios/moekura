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

/// Whether the account called `name` (any case) used an address in
/// `network`.
pub async fn name_used(db: impl PgExecutor<'_>, name: &str, network: IpNet) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM user_ips i JOIN users u ON u.id = i.user_id
             WHERE u.name = $1::citext AND i.ip <<= $2
         )",
    )
    .bind(name)
    .bind(network)
    .fetch_one(db)
    .await
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

/// Other accounts ranked below `below_rank` seen on the addresses
/// `user_id` used, most recently seen first.
pub async fn related(
    db: impl PgExecutor<'_>,
    user_id: i64,
    below_rank: i16,
    limit: i64,
) -> sqlx::Result<Vec<Related>> {
    sqlx::query_as(
        "SELECT u.name::text AS name, o.ip, o.last_seen_at
         FROM user_ips mine
         JOIN user_ips o ON o.ip = mine.ip AND o.user_id <> mine.user_id
         JOIN users u ON u.id = o.user_id
         JOIN roles r ON r.id = u.role_id
         WHERE mine.user_id = $1 AND r.rank < $2
         ORDER BY o.last_seen_at DESC LIMIT $3",
    )
    .bind(user_id)
    .bind(below_rank)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The highest rank among accounts seen on an address in `network`, as
/// recorded here or by a session still open; `None` if nobody was.
pub async fn top_rank_in(db: impl PgExecutor<'_>, network: IpNet) -> sqlx::Result<Option<i16>> {
    sqlx::query_scalar(
        "SELECT max(r.rank) FROM (
             SELECT user_id FROM user_ips WHERE ip <<= $1
             UNION
             SELECT user_id FROM sessions WHERE ip <<= $1 AND expires_at > now()
         ) seen
         JOIN users u ON u.id = seen.user_id
         JOIN roles r ON r.id = u.role_id",
    )
    .bind(network.trunc())
    .fetch_one(db)
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
    async fn finds_networks_a_name_used(pool: PgPool) {
        let alice = user(&pool, "Alice").await;
        user(&pool, "bob").await;
        record(&pool, alice, "2001:db8:1:2::5".parse().unwrap())
            .await
            .unwrap();
        record(&pool, alice, "198.51.100.7".parse().unwrap())
            .await
            .unwrap();
        let used = async |name: &str, network: &str| {
            name_used(&pool, name, network.parse().unwrap())
                .await
                .unwrap()
        };
        assert!(used("alice", "2001:db8:1:2::/64").await);
        assert!(used("ALICE", "198.51.100.7/32").await);
        assert!(!used("alice", "2001:db8:1:3::/64").await);
        assert!(!used("alice", "198.51.100.8/32").await);
        assert!(!used("bob", "198.51.100.7/32").await);
        assert!(!used("carol", "198.51.100.7/32").await);
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
        let found = related(&pool, alice, i16::MAX, 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].name.as_str(), found[0].ip.addr()),
            ("bob", shared)
        );
        // Not those ranked at or above whoever looks.
        let member: i16 = sqlx::query_scalar("SELECT rank FROM roles WHERE system_key = 'member'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(related(&pool, alice, member, 10).await.unwrap().is_empty());

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

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn the_highest_rank_seen_in_a_range(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let root: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'root', id FROM roles WHERE system_key = 'admin' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let (member, admin): (i16, i16) = sqlx::query_as(
            "SELECT (SELECT rank FROM roles WHERE system_key = 'member'),
                    (SELECT rank FROM roles WHERE system_key = 'admin')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let range = |text: &str| text.parse::<IpNet>().unwrap();
        record(&pool, alice, "203.0.113.7".parse().unwrap())
            .await
            .unwrap();
        assert_eq!(
            top_rank_in(&pool, range("203.0.113.0/24")).await.unwrap(),
            Some(member)
        );
        assert_eq!(
            top_rank_in(&pool, range("198.51.100.0/24")).await.unwrap(),
            None
        );
        // An open session counts, though the address isn't recorded.
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, expires_at, ip)
             VALUES ($1, $2, now() + interval '1 day', '203.0.113.9')",
        )
        .bind(vec![1u8; 32])
        .bind(root)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            top_rank_in(&pool, range("203.0.113.0/24")).await.unwrap(),
            Some(admin)
        );
        assert_eq!(
            top_rank_in(&pool, range("203.0.113.7/32")).await.unwrap(),
            Some(member)
        );
        sqlx::query("UPDATE sessions SET expires_at = now() - interval '1 second'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            top_rank_in(&pool, range("203.0.113.0/24")).await.unwrap(),
            Some(member)
        );
    }
}
