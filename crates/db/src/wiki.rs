//! Wiki pages and their history (`wiki_pages`, `wiki_page_versions`).

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct WikiPage {
    pub id: i32,
    pub title: String,
    pub body: String,
    /// What the tag is called elsewhere (moekura_core::wiki).
    pub other_names: Vec<String>,
    pub version: i32,
    pub updater_name: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A page in a list, without its text.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Summary {
    pub title: String,
    pub other_names: Vec<String>,
    pub version: i32,
    pub updater_name: Option<String>,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Version {
    pub version: i32,
    pub updater_name: Option<String>,
    pub body: String,
    pub other_names: Vec<String>,
    pub created_at: OffsetDateTime,
}

pub async fn by_title(db: impl PgExecutor<'_>, title: &str) -> sqlx::Result<Option<WikiPage>> {
    sqlx::query_as(
        "SELECT p.id, p.title, p.body, p.other_names, p.version, u.name::text AS updater_name,
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
        "SELECT p.id, p.title, p.body, p.other_names, p.version, u.name::text AS updater_name,
                p.created_at, p.updated_at
         FROM wiki_pages p LEFT JOIN users u ON u.id = p.updater_id
         WHERE p.id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Where [`list`] looks for its pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    Title,
    OtherNames,
    /// The title or any other name.
    Either,
}

/// Pages whose title or other names (as `matching` says) match `pattern`
/// (see [`crate::tags::like_pattern`]; other names regardless of case),
/// most recently changed first.
pub async fn list(
    db: impl PgExecutor<'_>,
    pattern: &str,
    matching: Match,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Summary>> {
    let like = crate::tags::like_pattern(pattern);
    let (title, other) = match matching {
        Match::Title => (true, false),
        Match::OtherNames => (false, true),
        Match::Either => (true, true),
    };
    sqlx::query_as(
        "SELECT p.title, p.other_names, p.version, u.name::text AS updater_name, p.updated_at
         FROM wiki_pages p LEFT JOIN users u ON u.id = p.updater_id
         WHERE ($1 = '%' AND $2)
            OR ($2 AND p.title LIKE $1)
            OR ($3 AND EXISTS (SELECT 1 FROM unnest(p.other_names) AS n WHERE lower(n) LIKE lower($1)))
         ORDER BY p.updated_at DESC, p.id DESC OFFSET $4 LIMIT $5",
    )
    .bind(like)
    .bind(title)
    .bind(other)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Titles of the pages with any of `names` among their other names
/// (exactly), by title.
pub async fn titles_for_other_names(
    db: impl PgExecutor<'_>,
    names: &[String],
) -> sqlx::Result<Vec<(String, Vec<String>)>> {
    sqlx::query_as(
        "SELECT title::text, other_names FROM wiki_pages
         WHERE other_names && $1::text[] ORDER BY title",
    )
    .bind(names)
    .fetch_all(db)
    .await
}

/// A page's versions, newest first (at most the latest 500).
pub async fn versions(db: impl PgExecutor<'_>, page_id: i32) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(
        "SELECT v.version, u.name::text AS updater_name, v.body, v.other_names, v.created_at
         FROM wiki_page_versions v LEFT JOIN users u ON u.id = v.updater_id
         WHERE v.wiki_page_id = $1 ORDER BY v.version DESC LIMIT 500",
    )
    .bind(page_id)
    .fetch_all(db)
    .await
}

/// A version in the sitewide list, with the length of the text before it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Change {
    pub id: i64,
    pub title: String,
    pub version: i32,
    pub updater_name: Option<String>,
    pub length: i32,
    /// None for a page's first version.
    pub previous_length: Option<i32>,
    pub created_at: OffsetDateTime,
}

/// Versions of every page, newest first, optionally only one user's,
/// older than version id `before`.
pub async fn recent_versions(
    db: impl PgExecutor<'_>,
    updater_id: Option<i64>,
    before: Option<i64>,
    limit: i64,
) -> sqlx::Result<Vec<Change>> {
    sqlx::query_as(
        "SELECT v.id, p.title::text AS title, v.version, u.name::text AS updater_name,
                length(v.body) AS length, length(pv.body) AS previous_length, v.created_at
         FROM wiki_page_versions v
         JOIN wiki_pages p ON p.id = v.wiki_page_id
         LEFT JOIN wiki_page_versions pv
             ON pv.wiki_page_id = v.wiki_page_id AND pv.version = v.version - 1
         LEFT JOIN users u ON u.id = v.updater_id
         WHERE ($1::bigint IS NULL OR v.updater_id = $1) AND ($2::bigint IS NULL OR v.id < $2)
         ORDER BY v.id DESC LIMIT $3",
    )
    .bind(updater_id)
    .bind(before)
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn version(
    db: impl PgExecutor<'_>,
    page_id: i32,
    version: i32,
) -> sqlx::Result<Option<Version>> {
    sqlx::query_as(
        "SELECT v.version, u.name::text AS updater_name, v.body, v.other_names, v.created_at
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

/// What a save writes to a page.
#[derive(Debug, Clone, Copy)]
pub struct Text<'a> {
    pub body: &'a str,
    /// Normalised (moekura_core::wiki); `None` keeps the page's.
    pub other_names: Option<&'a [String]>,
}

impl<'a> From<&'a str> for Text<'a> {
    /// Just the text, keeping the page's other names.
    fn from(body: &'a str) -> Self {
        Self {
            body,
            other_names: None,
        }
    }
}

/// Saves `text` to the page `title` (already normalised), creating it if
/// needed, and returns the page's version afterwards.
///
/// `base` is the version the editor started from, 0 for a new page; a
/// different current version is a [`SaveError::Conflict`]. `None` saves
/// over whatever is there. Saving unchanged text records no new version.
pub async fn save<'a>(
    db: &PgPool,
    title: &str,
    text: impl Into<Text<'a>>,
    updater_id: Option<i64>,
    base: Option<i32>,
) -> Result<i32, SaveError> {
    let text = text.into();
    let body = text.body;
    let mut tx = db.begin().await?;
    let existing: Option<(i32, i32, String, Vec<String>)> = sqlx::query_as(
        "SELECT id, version, body, other_names FROM wiki_pages WHERE title = $1 FOR UPDATE",
    )
    .bind(title)
    .fetch_optional(&mut *tx)
    .await?;
    let current = existing.as_ref().map_or(0, |(_, version, _, _)| *version);
    if let Some(base) = base
        && base != current
    {
        return Err(SaveError::Conflict { base, current });
    }
    let other_names: Vec<String> = match (text.other_names, &existing) {
        (Some(names), _) => names.to_vec(),
        (None, Some((_, _, _, names))) => names.clone(),
        (None, None) => Vec::new(),
    };
    let (id, version) = match existing {
        Some((_, version, old, old_names)) if old == body && old_names == other_names => {
            return Ok(version);
        }
        Some((id, version, _, _)) => {
            sqlx::query(
                "UPDATE wiki_pages SET body = $2, other_names = $3, version = $4, updater_id = $5,
                                       updated_at = now()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(body)
            .bind(&other_names)
            .bind(version + 1)
            .bind(updater_id)
            .execute(&mut *tx)
            .await?;
            (id, version + 1)
        }
        None => {
            // Someone creating the same page at the same moment wins.
            let id: Option<i32> = sqlx::query_scalar(
                "INSERT INTO wiki_pages (title, body, other_names, updater_id) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (title) DO NOTHING RETURNING id",
            )
            .bind(title)
            .bind(body)
            .bind(&other_names)
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
        "INSERT INTO wiki_page_versions (wiki_page_id, version, updater_id, body, other_names)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(version)
    .bind(updater_id)
    .bind(body)
    .bind(&other_names)
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
            titles(list(&pool, "cat", Match::Title, 0, 10).await.unwrap()),
            ["cat", "cat_ears"]
        );
        assert_eq!(
            titles(list(&pool, "*og", Match::Title, 0, 10).await.unwrap()),
            ["dog"]
        );
        assert_eq!(list(&pool, "", Match::Title, 0, 10).await.unwrap().len(), 3);
        assert_eq!(
            titles(list(&pool, "", Match::Title, 1, 1).await.unwrap()),
            ["cat"]
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn other_names(pool: PgPool) {
        let names = vec!["猫".to_owned(), "Neko".to_owned()];
        let text = Text {
            body: "A cat.",
            other_names: Some(&names),
        };
        assert_eq!(save(&pool, "cat", text, None, Some(0)).await.unwrap(), 1);
        save(&pool, "dog", "A dog.", None, None).await.unwrap();
        // Saving just the text keeps them; changing only them is a version.
        assert_eq!(save(&pool, "cat", "A cat.", None, None).await.unwrap(), 1);
        let page = by_title(&pool, "cat").await.unwrap().unwrap();
        assert_eq!(page.other_names, names);
        let fewer = vec!["Neko".to_owned()];
        let text = Text {
            body: "A cat.",
            other_names: Some(&fewer),
        };
        assert_eq!(save(&pool, "cat", text, None, None).await.unwrap(), 2);
        let history = versions(&pool, page.id).await.unwrap();
        assert_eq!(history[0].other_names, fewer);
        assert_eq!(history[1].other_names, names);

        let titles = |found: Vec<Summary>| found.into_iter().map(|s| s.title).collect::<Vec<_>>();
        assert_eq!(
            titles(list(&pool, "neko", Match::Either, 0, 10).await.unwrap()),
            ["cat"]
        );
        assert_eq!(
            titles(list(&pool, "d", Match::Either, 0, 10).await.unwrap()),
            ["dog"]
        );
        assert!(
            list(&pool, "cat", Match::OtherNames, 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            titles_for_other_names(&pool, &["Neko".to_owned(), "犬".to_owned()])
                .await
                .unwrap(),
            [("cat".to_owned(), fewer)]
        );
    }
}
