//! Requests to change tags: votes on alias and implication requests, bulk
//! update requests, and the discussion of either.

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

/// What a vote or comment is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// An alias or implication request (`tag_relations`).
    Relation(i32),
    /// A bulk update request.
    Request(i32),
}

impl Target {
    fn ids(self) -> (Option<i32>, Option<i32>) {
        match self {
            Target::Relation(id) => (Some(id), None),
            Target::Request(id) => (None, Some(id)),
        }
    }
}

/// Sets `user_id`'s vote on a target: `1`, `-1`, or `0` to take it back.
pub async fn vote(
    db: impl PgExecutor<'_>,
    target: Target,
    user_id: i64,
    score: i16,
) -> sqlx::Result<()> {
    let (sql, id) = match (target, score) {
        (Target::Relation(id), 0) => (
            "DELETE FROM tag_relation_votes WHERE relation_id = $1 AND user_id = $2 AND $3 = 0",
            id,
        ),
        (Target::Relation(id), _) => (
            "INSERT INTO tag_relation_votes (relation_id, user_id, score) VALUES ($1, $2, $3)
             ON CONFLICT (relation_id, user_id) DO UPDATE SET score = EXCLUDED.score
             WHERE tag_relation_votes.score <> EXCLUDED.score",
            id,
        ),
        (Target::Request(id), 0) => (
            "DELETE FROM bulk_update_request_votes WHERE request_id = $1 AND user_id = $2 AND $3 = 0",
            id,
        ),
        (Target::Request(id), _) => (
            "INSERT INTO bulk_update_request_votes (request_id, user_id, score) VALUES ($1, $2, $3)
             ON CONFLICT (request_id, user_id) DO UPDATE SET score = EXCLUDED.score
             WHERE bulk_update_request_votes.score <> EXCLUDED.score",
            id,
        ),
    };
    sqlx::query(sql)
        .bind(id)
        .bind(user_id)
        .bind(score)
        .execute(db)
        .await?;
    Ok(())
}

/// `user_id`'s vote on a target: 1, -1 or 0.
pub async fn vote_of(db: impl PgExecutor<'_>, target: Target, user_id: i64) -> sqlx::Result<i16> {
    let (relation, request) = target.ids();
    let vote: Option<i16> = sqlx::query_scalar(
        "SELECT score FROM tag_relation_votes WHERE relation_id = $1 AND user_id = $3
         UNION ALL
         SELECT score FROM bulk_update_request_votes WHERE request_id = $2 AND user_id = $3",
    )
    .bind(relation)
    .bind(request)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    Ok(vote.unwrap_or(0))
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Comment {
    pub id: i64,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub body: String,
    pub is_deleted: bool,
    pub created_at: OffsetDateTime,
}

pub async fn add_comment(
    db: impl PgExecutor<'_>,
    target: Target,
    creator_id: i64,
    body: &str,
) -> sqlx::Result<i64> {
    let (relation, request) = target.ids();
    sqlx::query_scalar(
        "INSERT INTO request_comments (relation_id, request_id, creator_id, body)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(relation)
    .bind(request)
    .bind(creator_id)
    .bind(body)
    .fetch_one(db)
    .await
}

/// A target's discussion, oldest first (at most 500).
pub async fn comments(
    db: impl PgExecutor<'_>,
    target: Target,
    with_deleted: bool,
) -> sqlx::Result<Vec<Comment>> {
    let (relation, request) = target.ids();
    sqlx::query_as(
        "SELECT c.id, c.creator_id, u.name::text AS creator_name, c.body, c.is_deleted, c.created_at
         FROM request_comments c LEFT JOIN users u ON u.id = c.creator_id
         WHERE (c.relation_id = $1 OR c.request_id = $2) AND ($3 OR NOT c.is_deleted)
         ORDER BY c.id LIMIT 500",
    )
    .bind(relation)
    .bind(request)
    .bind(with_deleted)
    .fetch_all(db)
    .await
}

/// Hides a comment of `creator_id`'s (or anyone's, for `None`); false if
/// there was none.
pub async fn delete_comment(
    db: impl PgExecutor<'_>,
    id: i64,
    creator_id: Option<i64>,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE request_comments SET is_deleted = true
         WHERE id = $1 AND ($2::bigint IS NULL OR creator_id = $2) AND NOT is_deleted",
    )
    .bind(id)
    .bind(creator_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Where a comment is: its target.
pub async fn comment_target(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Target>> {
    let row: Option<(Option<i32>, Option<i32>)> =
        sqlx::query_as("SELECT relation_id, request_id FROM request_comments WHERE id = $1")
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(row.and_then(|ids| match ids {
        (Some(id), _) => Some(Target::Relation(id)),
        (_, Some(id)) => Some(Target::Request(id)),
        _ => None,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BulkRequest {
    pub id: i32,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub title: String,
    pub script: String,
    pub reason: String,
    pub status: String,
    pub error: Option<String>,
    pub approver_id: Option<i64>,
    pub approver_name: Option<String>,
    pub score: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

macro_rules! select_requests {
    ($rest:literal) => {
        concat!(
            "SELECT b.id, b.creator_id, c.name::text AS creator_name, b.title, b.script, b.reason,
                    b.status, b.error, b.approver_id, a.name::text AS approver_name, b.score,
                    b.created_at, b.updated_at
             FROM bulk_update_requests b
             LEFT JOIN users c ON c.id = b.creator_id
             LEFT JOIN users a ON a.id = b.approver_id ",
            $rest
        )
    };
}

pub async fn create(
    db: impl PgExecutor<'_>,
    creator_id: i64,
    title: &str,
    script: &str,
    reason: &str,
) -> sqlx::Result<i32> {
    sqlx::query_scalar(
        "INSERT INTO bulk_update_requests (creator_id, title, script, reason)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(creator_id)
    .bind(title)
    .bind(script)
    .bind(reason)
    .fetch_one(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<Option<BulkRequest>> {
    sqlx::query_as(select_requests!("WHERE b.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Bulk update requests, newest first, optionally by status.
pub async fn list(
    db: impl PgExecutor<'_>,
    status: Option<&str>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<BulkRequest>> {
    sqlx::query_as(select_requests!(
        "WHERE ($1::text IS NULL OR b.status = $1) ORDER BY b.id DESC OFFSET $2 LIMIT $3"
    ))
    .bind(status)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Moves a request from status `from` to `to`, noting who decided;
/// false if it wasn't in `from`.
pub async fn set_status(
    db: impl PgExecutor<'_>,
    id: i32,
    from: &str,
    to: &str,
    approver_id: Option<i64>,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE bulk_update_requests
         SET status = $3, approver_id = coalesce($4, approver_id), updated_at = now()
         WHERE id = $1 AND status = $2",
    )
    .bind(id)
    .bind(from)
    .bind(to)
    .bind(approver_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Marks an applying request applied, or failed with `error`.
pub async fn finish(db: &PgPool, id: i32, error: Option<&str>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE bulk_update_requests
         SET status = CASE WHEN $2::text IS NULL THEN 'applied' ELSE 'failed' END,
             error = $2, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
    async fn votes_comments_and_requests(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let relation = crate::tag_relations::request(
            &pool,
            crate::tag_relations::NewRequest {
                kind: crate::tag_relations::Kind::Alias,
                antecedent: "kitty",
                consequent: "cat",
                reason: "",
                creator_id: Some(alice),
            },
        )
        .await
        .unwrap();
        let target = Target::Relation(relation);
        vote(&pool, target, alice, 1).await.unwrap();
        vote(&pool, target, bob, -1).await.unwrap();
        vote(&pool, target, bob, 1).await.unwrap();
        let score = |pool: PgPool| async move {
            crate::tag_relations::by_id(&pool, relation)
                .await
                .unwrap()
                .unwrap()
                .score
        };
        assert_eq!(score(pool.clone()).await, 2);
        vote(&pool, target, bob, 0).await.unwrap();
        assert_eq!(score(pool.clone()).await, 1);
        assert_eq!(vote_of(&pool, target, alice).await.unwrap(), 1);
        assert_eq!(vote_of(&pool, target, bob).await.unwrap(), 0);

        let id = create(&pool, alice, "Cats", "alias kitty -> cat", "tidy")
            .await
            .unwrap();
        let request = Target::Request(id);
        vote(&pool, request, bob, -1).await.unwrap();
        assert_eq!(by_id(&pool, id).await.unwrap().unwrap().score, -1);
        assert_eq!(vote_of(&pool, request, bob).await.unwrap(), -1);

        let comment = add_comment(&pool, request, bob, "Why?").await.unwrap();
        add_comment(&pool, target, bob, "Other").await.unwrap();
        assert_eq!(comments(&pool, request, false).await.unwrap().len(), 1);
        assert_eq!(comment_target(&pool, comment).await.unwrap(), Some(request));
        assert!(!delete_comment(&pool, comment, Some(alice)).await.unwrap());
        assert!(delete_comment(&pool, comment, Some(bob)).await.unwrap());
        assert!(comments(&pool, request, false).await.unwrap().is_empty());

        assert!(
            set_status(&pool, id, "pending", "approved", Some(bob))
                .await
                .unwrap()
        );
        assert!(
            !set_status(&pool, id, "pending", "rejected", Some(bob))
                .await
                .unwrap()
        );
        assert_eq!(list(&pool, Some("approved"), 0, 10).await.unwrap().len(), 1);
        finish(&pool, id, Some("broken")).await.unwrap();
        let failed = by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (failed.status.as_str(), failed.error.as_deref()),
            ("failed", Some("broken"))
        );
    }
}
