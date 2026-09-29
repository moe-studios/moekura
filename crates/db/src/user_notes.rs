//! Staff's private notes about users.

use sqlx::PgExecutor;
use time::OffsetDateTime;

/// The longest note, in characters.
pub const MAX_LEN: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserNote {
    pub id: i64,
    pub user_id: i64,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub body: String,
    pub created_at: OffsetDateTime,
}

pub async fn create(
    db: impl PgExecutor<'_>,
    user_id: i64,
    creator_id: i64,
    body: &str,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO user_notes (user_id, creator_id, body) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(user_id)
    .bind(creator_id)
    .bind(body)
    .fetch_one(db)
    .await
}

/// Notes about `user_id`, newest first.
pub async fn for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<UserNote>> {
    sqlx::query_as(
        "SELECT n.id, n.user_id, n.creator_id, c.name::text AS creator_name, n.body, n.created_at
         FROM user_notes n LEFT JOIN users c ON c.id = n.creator_id
         WHERE n.user_id = $1 ORDER BY n.id DESC",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<UserNote>> {
    sqlx::query_as(
        "SELECT n.id, n.user_id, n.creator_id, c.name::text AS creator_name, n.body, n.created_at
         FROM user_notes n LEFT JOIN users c ON c.id = n.creator_id WHERE n.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

pub async fn delete(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM user_notes WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() == 1)
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
    async fn notes_about_users(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let moderator = user(&pool, "mod").await;
        let first = create(&pool, alice, moderator, "warned about spam")
            .await
            .unwrap();
        create(&pool, alice, moderator, "again").await.unwrap();
        let notes = for_user(&pool, alice).await.unwrap();
        assert_eq!(
            notes.iter().map(|n| n.body.as_str()).collect::<Vec<_>>(),
            ["again", "warned about spam"]
        );
        assert_eq!(notes[0].creator_name.as_deref(), Some("mod"));
        assert!(for_user(&pool, moderator).await.unwrap().is_empty());
        assert!(delete(&pool, first).await.unwrap());
        assert!(by_id(&pool, first).await.unwrap().is_none());
        assert_eq!(for_user(&pool, alice).await.unwrap().len(), 1);
    }
}
