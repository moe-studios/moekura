//! Changing users' names, and remembering the old ones
//! (`user_name_changes`).

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct NameChange {
    pub id: i64,
    pub user_id: i64,
    pub old_name: String,
    pub new_name: String,
    pub changer_id: Option<i64>,
    pub created_at: OffsetDateTime,
}

/// `SELECT <name change columns> FROM user_name_changes` followed by
/// `$rest`.
macro_rules! select_changes {
    ($rest:literal) => {
        concat!(
            "SELECT id, user_id, old_name::text, new_name::text, changer_id, created_at
             FROM user_name_changes ",
            $rest
        )
    };
}

#[derive(Debug, thiserror::Error)]
pub enum RenameError {
    #[error("the name is taken")]
    Taken,
    #[error("no such user")]
    NoUser,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Renames user `user_id` to `new_name`, by `changer_id`, remembering the
/// old name. Changing only the case of the name is allowed.
pub async fn rename(
    db: &PgPool,
    user_id: i64,
    new_name: &str,
    changer_id: Option<i64>,
) -> Result<(), RenameError> {
    let mut tx = db.begin().await?;
    let old: String = sqlx::query_scalar("SELECT name::text FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RenameError::NoUser)?;
    let renamed = sqlx::query("UPDATE users SET name = $2 WHERE id = $1")
        .bind(user_id)
        .bind(new_name)
        .execute(&mut *tx)
        .await;
    match renamed {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => return Err(RenameError::Taken),
        other => other?,
    };
    sqlx::query(
        "INSERT INTO user_name_changes (user_id, old_name, new_name, changer_id)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(old)
    .bind(new_name)
    .bind(changer_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// `user_id`'s name changes, newest first.
pub async fn for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<NameChange>> {
    sqlx::query_as(select_changes!("WHERE user_id = $1 ORDER BY id DESC"))
        .bind(user_id)
        .fetch_all(db)
        .await
}

/// When `user_id` last changed their own name.
pub async fn last_own(
    db: impl PgExecutor<'_>,
    user_id: i64,
) -> sqlx::Result<Option<OffsetDateTime>> {
    sqlx::query_scalar(
        "SELECT max(created_at) FROM user_name_changes WHERE user_id = $1 AND changer_id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
    .await
}

/// Which name changes [`list`] returns.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter<'a> {
    pub user_id: Option<i64>,
    pub old_name: Option<&'a str>,
    pub new_name: Option<&'a str>,
    /// Only active users' changes.
    pub active_only: bool,
}

/// Name changes, newest first.
pub async fn list(
    db: impl PgExecutor<'_>,
    filter: &Filter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<NameChange>> {
    sqlx::query_as(select_changes!(
        "WHERE ($1::bigint IS NULL OR user_id = $1)
           AND ($2::citext IS NULL OR old_name = $2::citext)
           AND ($3::citext IS NULL OR new_name = $3::citext)
           AND (NOT $6 OR EXISTS (SELECT 1 FROM users u
                                  WHERE u.id = user_name_changes.user_id AND u.status = 'active'))
         ORDER BY id DESC LIMIT $4 OFFSET $5"
    ))
    .bind(filter.user_id)
    .bind(filter.old_name)
    .bind(filter.new_name)
    .bind(limit)
    .bind(offset)
    .bind(filter.active_only)
    .fetch_all(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<NameChange>> {
    sqlx::query_as(select_changes!("WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn renames_and_remembers(pool: PgPool) {
        let user = |name: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
                )
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let alice = user("alice").await;
        user("bob").await;
        assert!(matches!(
            rename(&pool, alice, "BOB", Some(alice)).await,
            Err(RenameError::Taken)
        ));
        rename(&pool, alice, "alicia", Some(alice)).await.unwrap();
        rename(&pool, alice, "Alicia", None).await.unwrap();
        let changes = for_user(&pool, alice).await.unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[1].old_name, "alice");
        assert!(last_own(&pool, alice).await.unwrap().is_some());
        let found = crate::users::by_name_or_former(&pool, "ALICE")
            .await
            .unwrap()
            .unwrap();
        assert_eq!((found.id, found.name.as_str()), (alice, "Alicia"));
        // A current name wins over a former one.
        rename(&pool, alice, "carol", Some(alice)).await.unwrap();
        let alicia = user("alicia").await;
        assert_eq!(
            crate::users::by_name_or_former(&pool, "alicia")
                .await
                .unwrap()
                .unwrap()
                .id,
            alicia
        );
        let by_old = list(
            &pool,
            &Filter {
                old_name: Some("ALICE"),
                ..Filter::default()
            },
            0,
            10,
        )
        .await
        .unwrap();
        assert_eq!(by_old.len(), 1);
    }
}
