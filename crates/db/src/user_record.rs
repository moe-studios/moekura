//! A user's record for staff: what they uploaded and how it fared, the
//! flags and reports on both sides, gathered for the moderation page.

use sqlx::PgExecutor;
use time::OffsetDateTime;

/// How many of something are in each status, e.g. uploads or flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally(pub Vec<(String, i64)>);

impl Tally {
    pub fn get(&self, status: &str) -> i64 {
        self.0
            .iter()
            .find(|(s, _)| s == status)
            .map_or(0, |(_, n)| *n)
    }

    pub fn total(&self) -> i64 {
        self.0.iter().map(|(_, n)| n).sum()
    }
}

async fn tally(db: impl PgExecutor<'_>, sql: &'static str, user_id: i64) -> sqlx::Result<Tally> {
    let rows: Vec<(String, i64)> = sqlx::query_as(sql).bind(user_id).fetch_all(db).await?;
    Ok(Tally(rows))
}

/// The user's uploads by post status.
pub async fn uploads(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Tally> {
    tally(
        db,
        "SELECT status, count(*) FROM posts WHERE uploader_id = $1 GROUP BY status",
        user_id,
    )
    .await
}

/// Flags on the user's uploads, by flag status.
pub async fn flags_received(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Tally> {
    tally(
        db,
        "SELECT f.status, count(*) FROM post_flags f JOIN posts p ON p.id = f.post_id
         WHERE p.uploader_id = $1 GROUP BY f.status",
        user_id,
    )
    .await
}

/// Flags the user filed, by flag status: upheld, dismissed or open.
pub async fn flags_filed(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Tally> {
    tally(
        db,
        "SELECT status, count(*) FROM post_flags WHERE creator_id = $1 GROUP BY status",
        user_id,
    )
    .await
}

/// Reports about the user's comments, by report status.
pub async fn reports_received(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Tally> {
    tally(
        db,
        "SELECT r.status, count(*) FROM comment_reports r JOIN comments c ON c.id = r.comment_id
         WHERE c.creator_id = $1 GROUP BY r.status",
        user_id,
    )
    .await
}

/// One of the user's uploads that was deleted, and why.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Deletion {
    pub post_id: i64,
    /// `post.delete` or `post.reject`; `None` if the log has no entry.
    pub action: Option<String>,
    pub reason: Option<String>,
    pub actor_name: Option<String>,
    pub deleted_at: Option<OffsetDateTime>,
}

/// The user's deleted uploads, most recently deleted first.
pub async fn deletions(
    db: impl PgExecutor<'_>,
    user_id: i64,
    limit: i64,
) -> sqlx::Result<Vec<Deletion>> {
    sqlx::query_as(
        "SELECT p.id AS post_id, m.action, m.reason, a.name::text AS actor_name,
                m.created_at AS deleted_at
         FROM posts p
         LEFT JOIN LATERAL (
             SELECT action, reason, actor_id, created_at FROM mod_actions
             WHERE post_id = p.id AND action IN ('post.delete', 'post.reject')
             ORDER BY id DESC LIMIT 1
         ) m ON true
         LEFT JOIN users a ON a.id = m.actor_id
         WHERE p.uploader_id = $1 AND p.status = 'deleted'
         ORDER BY m.created_at DESC NULLS LAST, p.id DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// A flag on one of the user's uploads.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ReceivedFlag {
    pub post_id: i64,
    pub creator_name: Option<String>,
    pub reason: String,
    pub status: String,
    pub created_at: OffsetDateTime,
}

/// The latest flags on the user's uploads, newest first.
pub async fn recent_flags_received(
    db: impl PgExecutor<'_>,
    user_id: i64,
    limit: i64,
) -> sqlx::Result<Vec<ReceivedFlag>> {
    sqlx::query_as(
        "SELECT f.post_id, c.name::text AS creator_name, f.reason, f.status, f.created_at
         FROM post_flags f JOIN posts p ON p.id = f.post_id
         LEFT JOIN users c ON c.id = f.creator_id
         WHERE p.uploader_id = $1 ORDER BY f.id DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// One of the user's comments that is hidden, with the reports on it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HiddenComment {
    pub id: i64,
    pub post_id: i64,
    pub body: String,
    pub created_at: OffsetDateTime,
    pub reports: i64,
}

/// The user's hidden comments, newest first.
pub async fn hidden_comments(
    db: impl PgExecutor<'_>,
    user_id: i64,
    limit: i64,
) -> sqlx::Result<Vec<HiddenComment>> {
    sqlx::query_as(
        "SELECT c.id, c.post_id, c.body, c.created_at,
                (SELECT count(*) FROM comment_reports r WHERE r.comment_id = c.id) AS reports
         FROM comments c WHERE c.creator_id = $1 AND c.is_deleted
         ORDER BY c.id DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Account facts the user record shows beside the `users` row.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Account {
    pub auto_promotion_blocked: bool,
    pub two_factor: bool,
}

pub async fn account(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Account> {
    sqlx::query_as(
        "SELECT u.auto_promotion_blocked,
                EXISTS (SELECT 1 FROM user_totp t WHERE t.user_id = u.id
                        AND t.enabled_at IS NOT NULL) AS two_factor
         FROM users u WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
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

    async fn post(pool: &PgPool, uploader: i64, status: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO posts (rating, status, uploader_id) VALUES ('g', $1, $2) RETURNING id",
        )
        .bind(status)
        .bind(uploader)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn gathers_a_record(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let kept = post(&pool, alice, "active").await;
        let gone = post(&pool, alice, "deleted").await;
        post(&pool, alice, "pending").await;
        sqlx::query(
            "INSERT INTO mod_actions (actor_id, action, post_id, reason)
             VALUES ($1, 'post.delete', $2, 'off-topic')",
        )
        .bind(bob)
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO post_flags (post_id, creator_id, reason, status)
             VALUES ($1, $2, 'dupe', 'dismissed'), ($3, $2, 'bad', 'upheld')",
        )
        .bind(kept)
        .bind(bob)
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();
        let comment: i64 = sqlx::query_scalar(
            "INSERT INTO comments (post_id, creator_id, body, is_deleted)
             VALUES ($1, $2, 'rude', true) RETURNING id",
        )
        .bind(kept)
        .bind(alice)
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO comment_reports (comment_id, creator_id, reason, status)
             VALUES ($1, $2, 'rude', 'upheld')",
        )
        .bind(comment)
        .bind(bob)
        .execute(&pool)
        .await
        .unwrap();

        let tally = uploads(&pool, alice).await.unwrap();
        assert_eq!(
            (tally.get("active"), tally.get("deleted"), tally.total()),
            (1, 1, 3)
        );
        let deleted = deletions(&pool, alice, 10).await.unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].reason.as_deref(), Some("off-topic"));
        assert_eq!(deleted[0].actor_name.as_deref(), Some("bob"));
        let received = flags_received(&pool, alice).await.unwrap();
        assert_eq!((received.get("upheld"), received.get("dismissed")), (1, 1));
        assert_eq!(
            recent_flags_received(&pool, alice, 10).await.unwrap().len(),
            2
        );
        let filed = flags_filed(&pool, bob).await.unwrap();
        assert_eq!(filed.total(), 2);
        assert!(flags_filed(&pool, alice).await.unwrap().0.is_empty());
        assert_eq!(
            reports_received(&pool, alice).await.unwrap().get("upheld"),
            1
        );
        let hidden = hidden_comments(&pool, alice, 10).await.unwrap();
        assert_eq!((hidden[0].id, hidden[0].reports), (comment, 1));
        assert_eq!(
            account(&pool, alice).await.unwrap(),
            Account {
                auto_promotion_blocked: false,
                two_factor: false
            }
        );
    }
}
