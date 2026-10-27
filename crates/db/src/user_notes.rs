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
    pub deleted_at: Option<OffsetDateTime>,
    pub deleter_name: Option<String>,
}

/// `SELECT <note columns> …` followed by `$rest`.
macro_rules! select_notes {
    ($rest:literal) => {
        concat!(
            "SELECT n.id, n.user_id, n.creator_id, c.name::text AS creator_name, n.body,
                    n.created_at, n.deleted_at, d.name::text AS deleter_name
             FROM user_notes n
             LEFT JOIN users c ON c.id = n.creator_id
             LEFT JOIN users d ON d.id = n.deleter_id ",
            $rest
        )
    };
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

/// Notes about `user_id`, newest first, with deleted ones if
/// `with_deleted`.
pub async fn for_user(
    db: impl PgExecutor<'_>,
    user_id: i64,
    with_deleted: bool,
) -> sqlx::Result<Vec<UserNote>> {
    sqlx::query_as(select_notes!(
        "WHERE n.user_id = $1 AND ($2 OR n.deleted_at IS NULL) ORDER BY n.id DESC"
    ))
    .bind(user_id)
    .bind(with_deleted)
    .fetch_all(db)
    .await
}

/// A note, deleted or not.
pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<UserNote>> {
    sqlx::query_as(select_notes!("WHERE n.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Deletes a note, keeping it with who deleted it; false if it was
/// already.
pub async fn delete(
    db: impl PgExecutor<'_>,
    id: i64,
    deleter_id: Option<i64>,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE user_notes SET deleted_at = now(), deleter_id = $2
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(deleter_id)
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
        let notes = for_user(&pool, alice, false).await.unwrap();
        assert_eq!(
            notes.iter().map(|n| n.body.as_str()).collect::<Vec<_>>(),
            ["again", "warned about spam"]
        );
        assert_eq!(notes[0].creator_name.as_deref(), Some("mod"));
        assert!(for_user(&pool, moderator, true).await.unwrap().is_empty());
        assert!(delete(&pool, first, Some(moderator)).await.unwrap());
        assert!(!delete(&pool, first, Some(moderator)).await.unwrap());
        let kept = by_id(&pool, first).await.unwrap().unwrap();
        assert!(kept.deleted_at.is_some());
        assert_eq!(kept.deleter_name.as_deref(), Some("mod"));
        assert_eq!(for_user(&pool, alice, false).await.unwrap().len(), 1);
        assert_eq!(for_user(&pool, alice, true).await.unwrap().len(), 2);
    }
}
