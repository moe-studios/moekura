//! Artist commentary on posts, with history (`artist_commentaries`,
//! `artist_commentary_versions`).

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

/// The four texts of a commentary.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Texts {
    pub original_title: String,
    pub original_description: String,
    pub translated_title: String,
    pub translated_description: String,
}

impl Texts {
    pub fn is_empty(&self) -> bool {
        self.original_title.is_empty()
            && self.original_description.is_empty()
            && self.translated_title.is_empty()
            && self.translated_description.is_empty()
    }

    pub fn is_translated(&self) -> bool {
        !(self.translated_title.is_empty() && self.translated_description.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Commentary {
    pub post_id: i64,
    #[sqlx(flatten)]
    pub texts: Texts,
    pub version: i32,
    pub updater_name: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// `SELECT <commentary columns> FROM artist_commentaries c LEFT JOIN
/// users u` followed by `$rest`.
macro_rules! select_commentaries {
    ($rest:literal) => {
        concat!(
            "SELECT c.post_id, c.original_title, c.original_description, c.translated_title,
                    c.translated_description, c.version, u.name::text AS updater_name,
                    c.created_at, c.updated_at
             FROM artist_commentaries c LEFT JOIN users u ON u.id = c.updater_id ",
            $rest
        )
    };
}

pub async fn for_post(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Option<Commentary>> {
    sqlx::query_as(select_commentaries!("WHERE c.post_id = $1"))
        .bind(post_id)
        .fetch_optional(db)
        .await
}

/// Which commentaries [`list`] returns.
#[derive(Debug, Clone, Default)]
pub struct Filter<'a> {
    pub post_ids: Option<&'a [i64]>,
    /// Words in any of the texts.
    pub text: &'a str,
    pub original_present: Option<bool>,
    pub translated_present: Option<bool>,
}

/// Commentaries matching `filter`, most recently changed first.
pub async fn list(
    db: impl PgExecutor<'_>,
    filter: &Filter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Commentary>> {
    sqlx::query_as(select_commentaries!(
        "WHERE ($1::bigint[] IS NULL OR c.post_id = ANY($1))
           AND ($2 = '' OR to_tsvector('simple', c.original_title || ' ' || c.original_description
                    || ' ' || c.translated_title || ' ' || c.translated_description)
                @@ plainto_tsquery('simple', $2))
           AND ($3::boolean IS NULL
                OR $3 = (c.original_title <> '' OR c.original_description <> ''))
           AND ($4::boolean IS NULL
                OR $4 = (c.translated_title <> '' OR c.translated_description <> ''))
         ORDER BY c.updated_at DESC, c.post_id DESC OFFSET $5 LIMIT $6"
    ))
    .bind(filter.post_ids)
    .bind(filter.text)
    .bind(filter.original_present)
    .bind(filter.translated_present)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// Someone else saved it since the editor loaded `base`.
    #[error("the commentary changed since version {base}; it is now at version {current}")]
    Conflict { base: i32, current: i32 },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Saves `texts` as post `post_id`'s commentary, creating it if needed,
/// and returns its version afterwards (0 when there was none and `texts`
/// are empty).
///
/// `base` is the version the editor started from, 0 for none; a different
/// current version is a [`SaveError::Conflict`]. `None` saves over
/// whatever is there. Saving what's there records no new version.
pub async fn save(
    db: &PgPool,
    post_id: i64,
    texts: &Texts,
    updater_id: Option<i64>,
    base: Option<i32>,
) -> Result<i32, SaveError> {
    let mut tx = db.begin().await?;
    // Serialises saves of a commentary that doesn't exist yet.
    sqlx::query("SELECT 1 FROM posts WHERE id = $1 FOR UPDATE")
        .bind(post_id)
        .execute(&mut *tx)
        .await?;
    let existing: Option<(i32, Texts)> =
        sqlx::query_as::<_, (i32, String, String, String, String)>(
            "SELECT version, original_title, original_description, translated_title,
                translated_description
         FROM artist_commentaries WHERE post_id = $1",
        )
        .bind(post_id)
        .fetch_optional(&mut *tx)
        .await?
        .map(|(v, a, b, c, d)| {
            (
                v,
                Texts {
                    original_title: a,
                    original_description: b,
                    translated_title: c,
                    translated_description: d,
                },
            )
        });
    let current = existing.as_ref().map_or(0, |(v, _)| *v);
    if let Some(base) = base
        && base != current
    {
        return Err(SaveError::Conflict { base, current });
    }
    match &existing {
        Some((version, old)) if old == texts => return Ok(*version),
        None if texts.is_empty() => return Ok(0),
        _ => {}
    }
    let version = current + 1;
    sqlx::query(
        "INSERT INTO artist_commentaries (post_id, original_title, original_description,
                                          translated_title, translated_description, version,
                                          updater_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (post_id) DO UPDATE SET
             original_title = EXCLUDED.original_title,
             original_description = EXCLUDED.original_description,
             translated_title = EXCLUDED.translated_title,
             translated_description = EXCLUDED.translated_description,
             version = EXCLUDED.version, updater_id = EXCLUDED.updater_id, updated_at = now()",
    )
    .bind(post_id)
    .bind(&texts.original_title)
    .bind(&texts.original_description)
    .bind(&texts.translated_title)
    .bind(&texts.translated_description)
    .bind(version)
    .bind(updater_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO artist_commentary_versions (post_id, version, updater_id, original_title,
             original_description, translated_title, translated_description)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(post_id)
    .bind(version)
    .bind(updater_id)
    .bind(&texts.original_title)
    .bind(&texts.original_description)
    .bind(&texts.translated_title)
    .bind(&texts.translated_description)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(version)
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Version {
    pub id: i64,
    pub post_id: i64,
    pub version: i32,
    pub updater_id: Option<i64>,
    pub updater_name: Option<String>,
    #[sqlx(flatten)]
    pub texts: Texts,
    pub created_at: OffsetDateTime,
}

/// Which versions [`versions`] returns.
#[derive(Debug, Clone, Copy, Default)]
pub struct VersionFilter {
    pub post_id: Option<i64>,
    pub updater_id: Option<i64>,
    /// Older than this version id.
    pub before: Option<i64>,
    pub offset: i64,
}

/// Versions, newest first.
pub async fn versions(
    db: impl PgExecutor<'_>,
    filter: VersionFilter,
    limit: i64,
) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(
        "SELECT v.id, v.post_id, v.version, v.updater_id, u.name::text AS updater_name,
                v.original_title, v.original_description, v.translated_title,
                v.translated_description, v.created_at
         FROM artist_commentary_versions v LEFT JOIN users u ON u.id = v.updater_id
         WHERE ($1::bigint IS NULL OR v.post_id = $1)
           AND ($2::bigint IS NULL OR v.updater_id = $2)
           AND ($3::bigint IS NULL OR v.id < $3)
         ORDER BY v.id DESC OFFSET $4 LIMIT $5",
    )
    .bind(filter.post_id)
    .bind(filter.updater_id)
    .bind(filter.before)
    .bind(filter.offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn post(pool: &PgPool) -> i64 {
        sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn saves_with_history(pool: PgPool) {
        let id = post(&pool).await;
        assert_eq!(
            save(&pool, id, &Texts::default(), None, Some(0))
                .await
                .unwrap(),
            0
        );
        assert!(for_post(&pool, id).await.unwrap().is_none());
        let original = Texts {
            original_title: "猫の絵".into(),
            original_description: "新しい絵です".into(),
            ..Texts::default()
        };
        assert_eq!(save(&pool, id, &original, None, Some(0)).await.unwrap(), 1);
        assert!(matches!(
            save(&pool, id, &original, None, Some(0)).await,
            Err(SaveError::Conflict {
                base: 0,
                current: 1
            })
        ));
        let translated = Texts {
            translated_title: "A cat picture".into(),
            ..original.clone()
        };
        assert_eq!(
            save(&pool, id, &translated, None, Some(1)).await.unwrap(),
            2
        );
        assert_eq!(save(&pool, id, &translated, None, None).await.unwrap(), 2);
        let saved = for_post(&pool, id).await.unwrap().unwrap();
        assert!(saved.texts.is_translated());

        let other = post(&pool).await;
        save(&pool, other, &original, None, None).await.unwrap();
        let found = |filter: Filter<'static>| {
            let pool = pool.clone();
            async move {
                list(&pool, &filter, 0, 10)
                    .await
                    .unwrap()
                    .iter()
                    .map(|c| c.post_id)
                    .collect::<Vec<_>>()
            }
        };
        assert_eq!(
            found(Filter {
                text: "cat",
                ..Filter::default()
            })
            .await,
            [id]
        );
        assert_eq!(
            found(Filter {
                translated_present: Some(false),
                ..Filter::default()
            })
            .await,
            [other]
        );
        let history = versions(
            &pool,
            VersionFilter {
                post_id: Some(id),
                ..VersionFilter::default()
            },
            10,
        )
        .await
        .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].texts, translated);
    }
}
