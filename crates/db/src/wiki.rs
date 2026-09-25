//! Wiki pages and their history (`wiki_pages`, `wiki_page_versions`).

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct WikiPage {
    pub id: i32,
    pub title: String,
    pub body: String,
    pub version: i32,
    pub updater_name: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A page in a list, without its text.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Summary {
    pub title: String,
    pub version: i32,
    pub updater_name: Option<String>,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Version {
    pub version: i32,
    pub updater_name: Option<String>,
    pub body: String,
    pub created_at: OffsetDateTime,
}

pub async fn by_title(db: impl PgExecutor<'_>, title: &str) -> sqlx::Result<Option<WikiPage>> {
    sqlx::query_as(
        "SELECT p.id, p.title, p.body, p.version, u.name::text AS updater_name,
                p.created_at, p.updated_at
         FROM wiki_pages p LEFT JOIN users u ON u.id = p.updater_id
         WHERE p.title = $1",
    )
    .bind(title)
    .fetch_optional(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<Option<WikiPage>> {
    sqlx::query_as(
        "SELECT p.id, p.title, p.body, p.version, u.name::text AS updater_name,
                p.created_at, p.updated_at
         FROM wiki_pages p LEFT JOIN users u ON u.id = p.updater_id
         WHERE p.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Pages whose title matches `pattern` (see [`crate::tags::like_pattern`]),
/// most recently changed first.
pub async fn list(
    db: impl PgExecutor<'_>,
    pattern: &str,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Summary>> {
    sqlx::query_as(
        "SELECT p.title, p.version, u.name::text AS updater_name, p.updated_at
         FROM wiki_pages p LEFT JOIN users u ON u.id = p.updater_id
         WHERE $1 = '%' OR p.title LIKE $1
         ORDER BY p.updated_at DESC, p.id DESC OFFSET $2 LIMIT $3",
    )
    .bind(crate::tags::like_pattern(pattern))
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// A page's versions, newest first (at most the latest 500).
pub async fn versions(db: impl PgExecutor<'_>, page_id: i32) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(
        "SELECT v.version, u.name::text AS updater_name, v.body, v.created_at
         FROM wiki_page_versions v LEFT JOIN users u ON u.id = v.updater_id
         WHERE v.wiki_page_id = $1 ORDER BY v.version DESC LIMIT 500",
    )
    .bind(page_id)
    .fetch_all(db)
    .await
}

pub async fn version(
    db: impl PgExecutor<'_>,
    page_id: i32,
    version: i32,
) -> sqlx::Result<Option<Version>> {
    sqlx::query_as(
        "SELECT v.version, u.name::text AS updater_name, v.body, v.created_at
         FROM wiki_page_versions v LEFT JOIN users u ON u.id = v.updater_id
         WHERE v.wiki_page_id = $1 AND v.version = $2",
    )
    .bind(page_id)
    .bind(version)
    .fetch_optional(db)
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// Someone else saved the page since the editor loaded `base`;
    /// `current` is the page's version now (0 if it doesn't exist).
    #[error("the page changed since version {base}; it is now at version {current}")]
    Conflict { base: i32, current: i32 },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Saves `body` as the text of the page `title` (already normalised),
/// creating it if needed, and returns the page's version afterwards.
///
/// `base` is the version the editor started from, 0 for a new page; a
/// different current version is a [`SaveError::Conflict`]. `None` saves
/// over whatever is there. Saving unchanged text records no new version.
pub async fn save(
    db: &PgPool,
    title: &str,
    body: &str,
    updater_id: Option<i64>,
    base: Option<i32>,
) -> Result<i32, SaveError> {
    let mut tx = db.begin().await?;
    let existing: Option<(i32, i32, String)> =
        sqlx::query_as("SELECT id, version, body FROM wiki_pages WHERE title = $1 FOR UPDATE")
            .bind(title)
            .fetch_optional(&mut *tx)
            .await?;
    let current = existing.as_ref().map_or(0, |(_, version, _)| *version);
    if let Some(base) = base
        && base != current
    {
        return Err(SaveError::Conflict { base, current });
    }
    let (id, version) = match existing {
        Some((_, version, old)) if old == body => return Ok(version),
        Some((id, version, _)) => {
            sqlx::query(
                "UPDATE wiki_pages SET body = $2, version = $3, updater_id = $4, updated_at = now()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(body)
            .bind(version + 1)
            .bind(updater_id)
            .execute(&mut *tx)
            .await?;
            (id, version + 1)
        }
        None => {
            // Someone creating the same page at the same moment wins.
            let id: Option<i32> = sqlx::query_scalar(
                "INSERT INTO wiki_pages (title, body, updater_id) VALUES ($1, $2, $3)
                 ON CONFLICT (title) DO NOTHING RETURNING id",
            )
            .bind(title)
            .bind(body)
            .bind(updater_id)
            .fetch_optional(&mut *tx)
            .await?;
            let id = id.ok_or(SaveError::Conflict {
                base: base.unwrap_or(0),
                current: 1,
            })?;
            (id, 1)
        }
    };
    sqlx::query(
        "INSERT INTO wiki_page_versions (wiki_page_id, version, updater_id, body)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(version)
    .bind(updater_id)
    .bind(body)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(version)
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
    async fn saves_versions_and_detects_conflicts(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;

        assert_eq!(
            save(&pool, "cat", "A cat.", Some(alice), Some(0))
                .await
                .unwrap(),
            1
        );
        // Unchanged text is not a new version.
        assert_eq!(
            save(&pool, "cat", "A cat.", Some(bob), Some(1))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            save(&pool, "cat", "A small cat.", Some(bob), Some(1))
                .await
                .unwrap(),
            2
        );
        // Alice's editor still has version 1.
        assert!(matches!(
            save(&pool, "cat", "A big cat.", Some(alice), Some(1)).await,
            Err(SaveError::Conflict {
                base: 1,
                current: 2
            })
        ));
        assert!(matches!(
            save(&pool, "cat", "Again.", Some(alice), Some(0)).await,
            Err(SaveError::Conflict {
                base: 0,
                current: 2
            })
        ));
        assert_eq!(
            save(&pool, "cat", "Overwritten.", None, None)
                .await
                .unwrap(),
            3
        );

        let page = by_title(&pool, "cat").await.unwrap().unwrap();
        assert_eq!(
            (page.body.as_str(), page.version, page.updater_name),
            ("Overwritten.", 3, None)
        );
        let history: Vec<_> = versions(&pool, page.id)
            .await
            .unwrap()
            .into_iter()
            .map(|v| (v.version, v.updater_name, v.body))
            .collect();
        assert_eq!(
            history,
            [
                (3, None, "Overwritten.".to_owned()),
                (2, Some("bob".to_owned()), "A small cat.".to_owned()),
                (1, Some("alice".to_owned()), "A cat.".to_owned()),
            ]
        );
        assert_eq!(
            version(&pool, page.id, 1).await.unwrap().unwrap().body,
            "A cat."
        );
        assert!(version(&pool, page.id, 4).await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn lists_by_title(pool: PgPool) {
        for title in ["cat_ears", "cat", "dog"] {
            save(&pool, title, "text", None, None).await.unwrap();
        }
        let titles = |found: Vec<Summary>| found.into_iter().map(|s| s.title).collect::<Vec<_>>();
        assert_eq!(
            titles(list(&pool, "cat", 0, 10).await.unwrap()),
            ["cat", "cat_ears"]
        );
        assert_eq!(titles(list(&pool, "*og", 0, 10).await.unwrap()), ["dog"]);
        assert_eq!(list(&pool, "", 0, 10).await.unwrap().len(), 3);
        assert_eq!(titles(list(&pool, "", 1, 1).await.unwrap()), ["cat"]);
    }
}
