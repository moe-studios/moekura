//! Searches users keep (`saved_searches`).

use sqlx::PgExecutor;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct SavedSearch {
    pub id: i64,
    pub query: String,
    pub labels: Vec<String>,
    pub created_at: OffsetDateTime,
}

/// A user's saved searches, by query.
pub async fn for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<SavedSearch>> {
    sqlx::query_as(
        "SELECT id, query, labels, created_at FROM saved_searches
         WHERE user_id = $1 ORDER BY query",
    )
    .bind(user_id)
    .fetch_all(db)
    .await
}

/// The queries a user saved with `label`, or all of them for `None`.
pub async fn queries(
    db: impl PgExecutor<'_>,
    user_id: i64,
    label: Option<&str>,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT query FROM saved_searches
         WHERE user_id = $1 AND ($2::text IS NULL OR $2 = ANY(labels))
         ORDER BY id",
    )
    .bind(user_id)
    .bind(label)
    .fetch_all(db)
    .await
}

pub async fn count(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM saved_searches WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// Saves a search, or replaces the labels of the same saved search.
pub async fn save(
    db: impl PgExecutor<'_>,
    user_id: i64,
    query: &str,
    labels: &[String],
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO saved_searches (user_id, query, labels) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, query) DO UPDATE SET labels = EXCLUDED.labels
         RETURNING id",
    )
    .bind(user_id)
    .bind(query)
    .bind(labels)
    .fetch_one(db)
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("You already saved that search.")]
    Duplicate,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Changes one of `user_id`'s saved searches; false if they have no such
/// search.
pub async fn update(
    db: impl PgExecutor<'_>,
    user_id: i64,
    id: i64,
    query: &str,
    labels: &[String],
) -> Result<bool, UpdateError> {
    let result = sqlx::query(
        "UPDATE saved_searches SET query = $3, labels = $4 WHERE id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .bind(query)
    .bind(labels)
    .execute(db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => UpdateError::Duplicate,
        _ => UpdateError::Db(e),
    })?;
    Ok(result.rows_affected() == 1)
}

/// Removes one of `user_id`'s saved searches; false if there was none.
pub async fn remove(db: impl PgExecutor<'_>, user_id: i64, id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM saved_searches WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
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
    async fn saves_lists_and_labels(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let labels = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let cats = save(&pool, alice, "cat", &labels(&["pets"])).await.unwrap();
        save(&pool, alice, "dog", &labels(&["pets", "dogs"]))
            .await
            .unwrap();
        save(&pool, alice, "tree", &[]).await.unwrap();
        // Saving again changes the labels.
        assert_eq!(
            save(&pool, alice, "cat", &labels(&["cats"])).await.unwrap(),
            cats
        );

        assert_eq!(queries(&pool, alice, Some("pets")).await.unwrap(), ["dog"]);
        assert_eq!(
            queries(&pool, alice, None).await.unwrap(),
            ["cat", "dog", "tree"]
        );
        assert!(queries(&pool, bob, None).await.unwrap().is_empty());
        assert_eq!(count(&pool, alice).await.unwrap(), 3);

        assert!(matches!(
            update(&pool, alice, cats, "dog", &[]).await,
            Err(UpdateError::Duplicate)
        ));
        assert!(!update(&pool, bob, cats, "x", &[]).await.unwrap());
        assert!(update(&pool, alice, cats, "cat ears", &[]).await.unwrap());
        assert!(!remove(&pool, bob, cats).await.unwrap());
        assert!(remove(&pool, alice, cats).await.unwrap());
        let left: Vec<String> = for_user(&pool, alice)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.query)
            .collect();
        assert_eq!(left, ["dog", "tree"]);
    }
}
