//! Tag history. Versions are recorded by triggers on `tags` (migration
//! 0055) when a tag is created and whenever its name, category or
//! deprecation changes, credited to the user
//! [`crate::post_versions::attribute`] names.

use sqlx::PgExecutor;
use time::OffsetDateTime;

/// A tag version with what the one before it had, for showing what
/// changed.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Change {
    pub id: i64,
    pub tag_id: i32,
    pub version: i32,
    pub updater_id: Option<i64>,
    pub updater_name: Option<String>,
    pub name: String,
    pub category_id: i16,
    pub is_deprecated: bool,
    pub created_at: OffsetDateTime,
    /// The previous version's id, if there is one (not so for the
    /// creation).
    pub previous_id: Option<i64>,
    pub previous_name: Option<String>,
    pub previous_category_id: Option<i16>,
    pub previous_is_deprecated: Option<bool>,
}

/// Which versions [`search`] finds; `None` matches any.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub tag_id: Option<i32>,
    pub updater_id: Option<i64>,
    /// Older than this version id (keyset pagination).
    pub before: Option<i64>,
    /// Versions skipped (the Danbooru API's pages).
    pub offset: i64,
}

/// Versions matching `filter`, newest first.
pub async fn search(
    db: impl PgExecutor<'_>,
    filter: &Filter,
    limit: i64,
) -> sqlx::Result<Vec<Change>> {
    sqlx::query_as(
        "SELECT v.id, v.tag_id, v.version, v.updater_id, u.name::text AS updater_name,
                v.name, v.category_id, v.is_deprecated, v.created_at,
                p.id AS previous_id, p.name AS previous_name,
                p.category_id AS previous_category_id,
                p.is_deprecated AS previous_is_deprecated
         FROM tag_versions v
         LEFT JOIN users u ON u.id = v.updater_id
         LEFT JOIN tag_versions p ON p.tag_id = v.tag_id AND p.version = v.version - 1
         WHERE ($1::int4 IS NULL OR v.tag_id = $1)
           AND ($2::int8 IS NULL OR v.updater_id = $2)
           AND ($3::int8 IS NULL OR v.id < $3)
         ORDER BY v.id DESC OFFSET $4 LIMIT $5",
    )
    .bind(filter.tag_id)
    .bind(filter.updater_id)
    .bind(filter.before)
    .bind(filter.offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::tags::{self, WantedTag};

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
    async fn records_creation_and_changes(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let mut tx = pool.begin().await.unwrap();
        crate::post_versions::attribute(&mut tx, Some(alice), None)
            .await
            .unwrap();
        let wanted = [
            WantedTag {
                name: "cat",
                category_id: None,
            },
            WantedTag {
                name: "someone",
                category_id: Some(1),
            },
        ];
        let created = tags::ensure(&mut tx, &wanted, false).await.unwrap();
        tx.commit().await.unwrap();
        let cat = created.iter().find(|t| t.name == "cat").unwrap().id;

        assert!(tags::update(&pool, cat, 4, true, Some(bob)).await.unwrap());
        // Unchanged saves and post counts aren't versions.
        assert!(tags::update(&pool, cat, 4, true, Some(bob)).await.unwrap());
        sqlx::query("UPDATE tags SET post_count = 3 WHERE id = $1")
            .bind(cat)
            .execute(&pool)
            .await
            .unwrap();

        let filter = Filter {
            tag_id: Some(cat),
            ..Filter::default()
        };
        let found = search(&pool, &filter, 10).await.unwrap();
        assert_eq!(found.len(), 2);
        let (latest, first) = (&found[0], &found[1]);
        assert_eq!(
            (latest.version, latest.updater_name.as_deref()),
            (2, Some("bob"))
        );
        assert_eq!((latest.category_id, latest.is_deprecated), (4, true));
        assert_eq!(
            (latest.previous_category_id, latest.previous_is_deprecated),
            (Some(0), Some(false))
        );
        assert_eq!(latest.previous_id, Some(first.id));
        assert_eq!(
            (first.version, first.updater_id, first.previous_id),
            (1, Some(alice), None)
        );

        let by_alice = Filter {
            updater_id: Some(alice),
            ..Filter::default()
        };
        assert_eq!(search(&pool, &by_alice, 10).await.unwrap().len(), 2);
        let older = Filter {
            before: Some(latest.id),
            tag_id: Some(cat),
            ..Filter::default()
        };
        assert_eq!(
            search(&pool, &older, 10).await.unwrap(),
            std::slice::from_ref(first)
        );
    }
}
