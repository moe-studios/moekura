//! Finding and promoting members whose record meets the promotion rules.

use moekura_core::moderation::ActionKind;
use moekura_core::permissions::SystemRole;
use moekura_core::promotion::{Record, Rules};
use sqlx::PgPool;

use crate::mod_actions::{self, NewAction};

/// A member who may be promoted, and their record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub user_id: i64,
    pub name: String,
    pub record: Record,
}

/// Active, unbanned members (of the built-in Member role) not kept from
/// promotion, whose record meets `rules`.
pub async fn candidates(db: &PgPool, rules: &Rules) -> sqlx::Result<Vec<Candidate>> {
    let rows: Vec<(i64, String, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT u.id, u.name::text,
                (SELECT count(*) FROM posts p
                 WHERE p.uploader_id = u.id AND p.status IN ('active', 'flagged')),
                (SELECT count(*) FROM post_versions v
                 WHERE v.updater_id = u.id AND v.version > 1 AND v.relation_id IS NULL),
                extract(day FROM now() - u.created_at)::bigint,
                (SELECT count(*) FROM posts p JOIN mod_actions m
                     ON m.post_id = p.id AND m.action = 'post.delete'
                 WHERE p.uploader_id = u.id AND p.status = 'deleted'
                   AND m.created_at > now() - interval '30 days')
         FROM users u JOIN roles r ON r.id = u.role_id
         WHERE r.system_key = 'member' AND u.status = 'active' AND NOT u.auto_promotion_blocked
           AND u.created_at <= now() - make_interval(days => $1)
           AND NOT EXISTS (SELECT 1 FROM bans b WHERE b.user_id = u.id AND b.lifted_at IS NULL
                           AND (b.expires_at IS NULL OR b.expires_at > now()))",
    )
    .bind(i32::try_from(rules.account_days).unwrap_or(i32::MAX))
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(user_id, name, uploads, edits, account_days, recent_deletions)| Candidate {
                user_id,
                name,
                record: Record {
                    uploads,
                    edits,
                    account_days,
                    recent_deletions,
                },
            },
        )
        .filter(|c| rules.met_by(&c.record))
        .collect())
}

/// Moves `candidate` from Member to Contributor, if they're still a
/// member, and logs it. Returns whether they were promoted.
pub async fn promote(db: &PgPool, candidate: &Candidate) -> sqlx::Result<bool> {
    let mut tx = db.begin().await?;
    let promoted = sqlx::query(
        "UPDATE users SET role_id = (SELECT id FROM roles WHERE system_key = $2)
         WHERE id = $1 AND role_id = (SELECT id FROM roles WHERE system_key = $3)",
    )
    .bind(candidate.user_id)
    .bind(SystemRole::Contributor.key())
    .bind(SystemRole::Member.key())
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    if promoted {
        let record = &candidate.record;
        mod_actions::record(
            &mut *tx,
            NewAction::new(None, ActionKind::UserPromote)
                .user(candidate.user_id)
                .reason("automatic promotion")
                .details(serde_json::json!({
                    "to": "contributor",
                    "uploads": record.uploads,
                    "edits": record.edits,
                    "account_days": record.account_days,
                })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(promoted)
}

/// Whether staff kept a user from automatic promotion, and when they were
/// last promoted automatically.
pub async fn status(
    db: impl sqlx::PgExecutor<'_>,
    user_id: i64,
) -> sqlx::Result<(bool, Option<time::OffsetDateTime>)> {
    sqlx::query_as(
        "SELECT u.auto_promotion_blocked,
                (SELECT max(m.created_at) FROM mod_actions m
                 WHERE m.user_id = u.id AND m.action = 'user.promote')
         FROM users u WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

/// Keeps a user from (or allows) automatic promotion.
pub async fn set_blocked(db: &PgPool, user_id: i64, blocked: bool) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET auto_promotion_blocked = $2 WHERE id = $1")
        .bind(user_id)
        .bind(blocked)
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn user(pool: &PgPool, name: &str, role: &str, days_old: i32) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id, created_at)
             SELECT $1, id, now() - make_interval(days => $3) FROM roles WHERE system_key = $2
             RETURNING id",
        )
        .bind(name)
        .bind(role)
        .bind(days_old)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn uploads(pool: &PgPool, user: i64, n: i32, status: &str) {
        sqlx::query(
            "INSERT INTO posts (rating, status, uploader_id) SELECT 'g', $2, $1 FROM generate_series(1, $3)",
        )
        .bind(user)
        .bind(status)
        .bind(n)
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn promotes_members_who_qualify(pool: PgPool) {
        let rules = Rules {
            uploads: 3,
            edits: 0,
            account_days: 7,
            max_recent_deletions: 0,
        };
        let good = user(&pool, "good", "member", 10).await;
        let young = user(&pool, "young", "member", 1).await;
        let few = user(&pool, "few", "member", 10).await;
        let blocked = user(&pool, "blocked", "member", 10).await;
        let janitor = user(&pool, "jan", "janitor", 10).await;
        for u in [good, young, blocked, janitor] {
            uploads(&pool, u, 3, "active").await;
        }
        uploads(&pool, few, 2, "active").await;
        uploads(&pool, few, 5, "pending").await;
        set_blocked(&pool, blocked, true).await.unwrap();

        let found = candidates(&pool, &rules).await.unwrap();
        assert_eq!(
            found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["good"]
        );
        assert!(promote(&pool, &found[0]).await.unwrap());
        // Already promoted: nothing to do.
        assert!(!promote(&pool, &found[0]).await.unwrap());
        let role: String = sqlx::query_scalar(
            "SELECT r.system_key FROM users u JOIN roles r ON r.id = u.role_id WHERE u.id = $1",
        )
        .bind(good)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(role, "contributor");
        let logged: String =
            sqlx::query_scalar("SELECT action FROM mod_actions WHERE user_id = $1")
                .bind(good)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(logged, "user.promote");
        assert!(candidates(&pool, &rules).await.unwrap().is_empty());

        // A recent deletion stops a promotion.
        let deleted = user(&pool, "deleted", "member", 10).await;
        uploads(&pool, deleted, 3, "active").await;
        let post: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, status, uploader_id) VALUES ('g', 'deleted', $1) RETURNING id",
        )
        .bind(deleted)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO mod_actions (action, post_id) VALUES ('post.delete', $1)")
            .bind(post)
            .execute(&pool)
            .await
            .unwrap();
        assert!(candidates(&pool, &rules).await.unwrap().is_empty());
    }
}
