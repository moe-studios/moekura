//! Post history. Versions are recorded by triggers on `posts` (migration
//! 0012) whenever tags, rating, source, description or parent change;
//! [`attribute`] tells them who is making the change.

use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Version {
    pub version: i32,
    pub updater_name: Option<String>,
    /// For changes made by a tag alias or implication.
    pub relation_kind: Option<String>,
    pub relation_antecedent: Option<String>,
    pub relation_consequent: Option<String>,
    pub tag_ids: Vec<i32>,
    pub added_tag_ids: Vec<i32>,
    pub removed_tag_ids: Vec<i32>,
    pub rating: String,
    pub source: String,
    pub description: String,
    pub parent_id: Option<i64>,
    pub created_at: OffsetDateTime,
}

/// `SELECT <version columns> …` followed by `$rest`.
macro_rules! select_versions {
    ($rest:literal) => {
        concat!(
            "SELECT v.version, u.name::text AS updater_name, r.kind AS relation_kind,
                    r.antecedent_name::text AS relation_antecedent,
                    r.consequent_name::text AS relation_consequent,
                    v.tag_ids, v.added_tag_ids, v.removed_tag_ids, v.rating, v.source,
                    v.description, v.parent_id, v.created_at
             FROM post_versions v
             LEFT JOIN users u ON u.id = v.updater_id
             LEFT JOIN tag_relations r ON r.id = v.relation_id ",
            $rest
        )
    };
}

/// Attributes post changes in the current transaction to a user, or to a
/// tag relation. Call it inside the transaction making the change.
pub async fn attribute(
    conn: &mut PgConnection,
    updater_id: Option<i64>,
    relation_id: Option<i32>,
) -> sqlx::Result<()> {
    let text = |v: Option<String>| v.unwrap_or_default();
    sqlx::query(
        "SELECT set_config('uwu.updater_id', $1, true), set_config('uwu.relation_id', $2, true)",
    )
    .bind(text(updater_id.map(|id| id.to_string())))
    .bind(text(relation_id.map(|id| id.to_string())))
    .execute(conn)
    .await?;
    Ok(())
}

/// A post's versions, newest first (at most the latest 500).
pub async fn list(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(select_versions!(
        "WHERE v.post_id = $1 ORDER BY v.version DESC LIMIT 500"
    ))
    .bind(post_id)
    .fetch_all(db)
    .await
}

pub async fn get(
    db: impl PgExecutor<'_>,
    post_id: i64,
    version: i32,
) -> sqlx::Result<Option<Version>> {
    sqlx::query_as(select_versions!("WHERE v.post_id = $1 AND v.version = $2"))
        .bind(post_id)
        .bind(version)
        .fetch_optional(db)
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

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn changes_are_recorded_and_attributed(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let ids = crate::tags::tests::create(&pool, &["a", "b", "c"]).await;
        let post: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, uploader_id, tag_ids) VALUES ('g', $1, $2) RETURNING id",
        )
        .bind(alice)
        .bind(&ids[..2])
        .fetch_one(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        attribute(&mut tx, Some(bob), None).await.unwrap();
        sqlx::query("UPDATE posts SET tag_ids = $2, rating = 'e' WHERE id = $1")
            .bind(post)
            .bind(&ids[1..])
            .execute(&mut *tx)
            .await
            .unwrap();
        // Changes to other columns aren't versions.
        sqlx::query("UPDATE posts SET score = 3 WHERE id = $1")
            .bind(post)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // Attribution ends with the transaction.
        sqlx::query("UPDATE posts SET source = 'x' WHERE id = $1")
            .bind(post)
            .execute(&pool)
            .await
            .unwrap();

        let versions = list(&pool, post).await.unwrap();
        let summary: Vec<_> = versions
            .iter()
            .map(|v| {
                (
                    v.version,
                    v.updater_name.as_deref(),
                    v.added_tag_ids.clone(),
                    v.removed_tag_ids.clone(),
                    v.rating.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (3, None, vec![], vec![], "e"),
                (2, Some("bob"), vec![ids[2]], vec![ids[0]], "e"),
                (1, Some("alice"), ids[..2].to_vec(), vec![], "g"),
            ]
        );
        assert_eq!(get(&pool, post, 3).await.unwrap().unwrap().source, "x");
        assert!(get(&pool, post, 4).await.unwrap().is_none());
    }
}
