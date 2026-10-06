//! Feedback left on users (`user_feedbacks`): positive, neutral or
//! negative, by staff and senior users.

use sqlx::PgExecutor;
use time::OffsetDateTime;

/// The categories, best first.
pub const CATEGORIES: [&str; 3] = ["positive", "neutral", "negative"];

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Feedback {
    pub id: i64,
    pub user_id: i64,
    pub user_name: String,
    /// The roles of whom it's on and who wrote it, for rank checks.
    pub user_role_id: i32,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub creator_role_id: Option<i32>,
    pub category: String,
    pub body: String,
    pub is_deleted: bool,
    /// Who deleted it and their role, while it's deleted.
    pub deleted_by_id: Option<i64>,
    pub deleted_by_role_id: Option<i32>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// `SELECT <feedback columns> FROM user_feedbacks f …` followed by `$rest`.
macro_rules! select_feedbacks {
    ($rest:literal) => {
        concat!(
            "SELECT f.id, f.user_id, u.name::text AS user_name, u.role_id AS user_role_id,
                    f.creator_id, c.name::text AS creator_name, c.role_id AS creator_role_id,
                    f.category, f.body, f.is_deleted,
                    f.deleted_by_id, d.role_id AS deleted_by_role_id, f.created_at, f.updated_at
             FROM user_feedbacks f JOIN users u ON u.id = f.user_id
             LEFT JOIN users c ON c.id = f.creator_id
             LEFT JOIN users d ON d.id = f.deleted_by_id ",
            $rest
        )
    };
}

/// Which feedback [`list`] returns.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter<'a> {
    pub user_id: Option<i64>,
    pub creator_id: Option<i64>,
    pub category: Option<&'a str>,
    pub with_deleted: bool,
}

/// Feedback, newest first.
pub async fn list(
    db: impl PgExecutor<'_>,
    filter: &Filter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Feedback>> {
    sqlx::query_as(select_feedbacks!(
        "WHERE ($1::bigint IS NULL OR f.user_id = $1)
           AND ($2::bigint IS NULL OR f.creator_id = $2)
           AND ($3::text IS NULL OR f.category = $3)
           AND ($4 OR NOT f.is_deleted)
         ORDER BY f.id DESC LIMIT $5 OFFSET $6"
    ))
    .bind(filter.user_id)
    .bind(filter.creator_id)
    .bind(filter.category)
    .bind(filter.with_deleted)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Feedback>> {
    sqlx::query_as(select_feedbacks!("WHERE f.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Feedback `id`, locked until the transaction ends.
pub async fn lock(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Feedback>> {
    sqlx::query_as(select_feedbacks!("WHERE f.id = $1 FOR UPDATE OF f"))
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn create(
    db: impl PgExecutor<'_>,
    user_id: i64,
    creator_id: i64,
    category: &str,
    body: &str,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO user_feedbacks (user_id, creator_id, category, body)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(user_id)
    .bind(creator_id)
    .bind(category)
    .bind(body)
    .fetch_one(db)
    .await
}

pub async fn update(
    db: impl PgExecutor<'_>,
    id: i64,
    category: &str,
    body: &str,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE user_feedbacks SET category = $2, body = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(category)
    .bind(body)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Deletes or restores feedback, by `actor_id`; false if it already was.
pub async fn set_deleted(
    db: impl PgExecutor<'_>,
    id: i64,
    deleted: bool,
    actor_id: Option<i64>,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE user_feedbacks SET is_deleted = $2, deleted_by_id = CASE WHEN $2 THEN $3 END
         WHERE id = $1 AND is_deleted <> $2",
    )
    .bind(id)
    .bind(deleted)
    .bind(actor_id)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// How much positive, neutral and negative feedback `user_id` has (not
/// deleted).
pub async fn counts(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<[i64; 3]> {
    let (positive, neutral, negative) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE category = 'positive'),
                count(*) FILTER (WHERE category = 'neutral'),
                count(*) FILTER (WHERE category = 'negative')
         FROM user_feedbacks WHERE user_id = $1 AND NOT is_deleted",
    )
    .bind(user_id)
    .fetch_one(db)
    .await?;
    Ok([positive, neutral, negative])
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    async fn user(pool: &PgPool, name: &str, role: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = $2 RETURNING id",
        )
        .bind(name)
        .bind(role)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn deleting_remembers_who_did(pool: PgPool) {
        let member = user(&pool, "member", "member").await;
        let moderator = user(&pool, "moderator", "moderator").await;
        let admin = user(&pool, "admin", "admin").await;
        let id = create(&pool, member, moderator, "negative", "Rude")
            .await
            .unwrap();

        assert!(set_deleted(&pool, id, true, Some(admin)).await.unwrap());
        assert!(!set_deleted(&pool, id, true, Some(member)).await.unwrap());
        let mut tx = pool.begin().await.unwrap();
        let f = lock(&mut *tx, id).await.unwrap().unwrap();
        tx.commit().await.unwrap();
        assert!(f.is_deleted);
        assert_eq!(f.deleted_by_id, Some(admin));
        let admin_role: i32 = sqlx::query_scalar("SELECT role_id FROM users WHERE id = $1")
            .bind(admin)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(f.deleted_by_role_id, Some(admin_role));

        // Restored, nobody deleted it.
        assert!(set_deleted(&pool, id, false, Some(admin)).await.unwrap());
        let f = by_id(&pool, id).await.unwrap().unwrap();
        assert!(!f.is_deleted);
        assert_eq!((f.deleted_by_id, f.deleted_by_role_id), (None, None));
    }
}
