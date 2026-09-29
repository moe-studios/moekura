//! Approvers' disapprovals of pending posts.

use moekura_core::moderation::DisapprovalReason;
use sqlx::PgExecutor;
use time::OffsetDateTime;

/// Records that `user_id` passes on pending post `post_id`, replacing
/// their earlier disapproval of it. False if the post isn't pending.
pub async fn disapprove(
    db: impl PgExecutor<'_>,
    post_id: i64,
    user_id: i64,
    reason: DisapprovalReason,
    message: &str,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "INSERT INTO post_disapprovals (post_id, user_id, reason, message)
         SELECT id, $2, $3, $4 FROM posts WHERE id = $1 AND status = 'pending'
         ON CONFLICT (post_id, user_id) DO UPDATE
         SET reason = excluded.reason, message = excluded.message, created_at = now()",
    )
    .bind(post_id)
    .bind(user_id)
    .bind(reason.as_str())
    .bind(message)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Takes back `user_id`'s disapproval of `post_id`; false if there was
/// none.
pub async fn withdraw(db: impl PgExecutor<'_>, post_id: i64, user_id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM post_disapprovals WHERE post_id = $1 AND user_id = $2")
        .bind(post_id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() == 1)
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Disapproval {
    pub post_id: i64,
    pub user_id: i64,
    pub user_name: String,
    pub reason: String,
    pub message: String,
    pub created_at: OffsetDateTime,
}

impl Disapproval {
    pub fn reason(&self) -> Option<DisapprovalReason> {
        DisapprovalReason::parse(&self.reason)
    }
}

/// The disapprovals of `post_ids`, oldest first.
pub async fn for_posts(
    db: impl PgExecutor<'_>,
    post_ids: &[i64],
) -> sqlx::Result<Vec<Disapproval>> {
    sqlx::query_as(
        "SELECT d.post_id, d.user_id, u.name::text AS user_name, d.reason, d.message, d.created_at
         FROM post_disapprovals d JOIN users u ON u.id = d.user_id
         WHERE d.post_id = ANY($1) ORDER BY d.created_at",
    )
    .bind(post_ids)
    .fetch_all(db)
    .await
}

/// How many pending posts `user_id` has disapproved, by reason.
pub async fn counts_by(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as(
        "SELECT reason, count(*) FROM post_disapprovals WHERE user_id = $1 GROUP BY reason",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'janitor' RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn disapproving_pending_posts(pool: PgPool) {
        let jan = user(&pool, "jan").await;
        let kim = user(&pool, "kim").await;
        let pending: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, status) VALUES ('g', 'pending') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let active: i64 =
            sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            disapprove(
                &pool,
                pending,
                jan,
                DisapprovalReason::PoorQuality,
                "blurry"
            )
            .await
            .unwrap()
        );
        // Again replaces the first.
        assert!(
            disapprove(&pool, pending, jan, DisapprovalReason::Disinterest, "")
                .await
                .unwrap()
        );
        disapprove(&pool, pending, kim, DisapprovalReason::BreaksRules, "")
            .await
            .unwrap();
        assert!(
            !disapprove(&pool, active, jan, DisapprovalReason::Disinterest, "")
                .await
                .unwrap(),
            "only pending posts"
        );
        let all = for_posts(&pool, &[pending, active]).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].user_name, "jan");
        assert_eq!(all[0].reason(), Some(DisapprovalReason::Disinterest));
        assert_eq!(
            counts_by(&pool, jan).await.unwrap(),
            [("disinterest".to_owned(), 1)]
        );
        assert!(withdraw(&pool, pending, jan).await.unwrap());
        assert!(!withdraw(&pool, pending, jan).await.unwrap());
    }
}
