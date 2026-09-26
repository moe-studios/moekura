//! Queries on `tagger_results` and `tag_suggestions`: what the tagger
//! made of posts.

use moekura_core::posts::Rating;
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

use crate::tags::{Tag, WantedTag};

/// The tagger's verdict on a post.
#[derive(Debug, Clone, PartialEq)]
pub struct TaggerResult {
    pub post_id: i64,
    pub model: String,
    pub rating: Rating,
    pub rating_confidence: f32,
    pub tagged_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct ResultRow {
    post_id: i64,
    model: String,
    rating: String,
    rating_confidence: f32,
    tagged_at: OffsetDateTime,
}

impl TryFrom<ResultRow> for TaggerResult {
    type Error = sqlx::Error;

    fn try_from(row: ResultRow) -> sqlx::Result<Self> {
        let rating = row
            .rating
            .parse()
            .map_err(|()| sqlx::Error::Decode(format!("unknown rating `{}`", row.rating).into()))?;
        Ok(Self {
            post_id: row.post_id,
            model: row.model,
            rating,
            rating_confidence: row.rating_confidence,
            tagged_at: row.tagged_at,
        })
    }
}

/// What to save for a post.
#[derive(Debug, Clone, PartialEq)]
pub struct NewResult<'a> {
    pub post_id: i64,
    pub model: &'a str,
    pub rating: Rating,
    pub rating_confidence: f32,
    /// Tag ids with the model's confidence, without duplicates.
    pub suggestions: &'a [(i32, f32)],
}

/// Replaces what was saved for the post before. Returns false, saving
/// nothing, if the post no longer exists.
pub async fn save(conn: &mut PgConnection, result: &NewResult<'_>) -> sqlx::Result<bool> {
    let saved = sqlx::query(
        "INSERT INTO tagger_results (post_id, model, rating, rating_confidence)
         SELECT id, $2, $3, $4 FROM posts WHERE id = $1
         ON CONFLICT (post_id) DO UPDATE
         SET model = excluded.model, rating = excluded.rating,
             rating_confidence = excluded.rating_confidence, tagged_at = now()",
    )
    .bind(result.post_id)
    .bind(result.model)
    .bind(result.rating.code())
    .bind(result.rating_confidence)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1;
    if !saved {
        return Ok(false);
    }
    sqlx::query("DELETE FROM tag_suggestions WHERE post_id = $1")
        .bind(result.post_id)
        .execute(&mut *conn)
        .await?;
    let (tag_ids, confidences): (Vec<i32>, Vec<f32>) = result.suggestions.iter().copied().unzip();
    sqlx::query(
        "INSERT INTO tag_suggestions (post_id, tag_id, confidence, model)
         SELECT $1, tag_id, confidence, $2
         FROM unnest($3::int4[], $4::real[]) AS s (tag_id, confidence)
         ON CONFLICT DO NOTHING",
    )
    .bind(result.post_id)
    .bind(result.model)
    .bind(&tag_ids)
    .bind(&confidences)
    .execute(&mut *conn)
    .await?;
    Ok(true)
}

pub async fn result(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Option<TaggerResult>> {
    let row: Option<ResultRow> = sqlx::query_as(
        "SELECT post_id, model, rating, rating_confidence, tagged_at
         FROM tagger_results WHERE post_id = $1",
    )
    .bind(post_id)
    .fetch_optional(db)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// The site's tags for names a model uses, with their confidence: aliases
/// resolved, and missing tags created in the model's category. Deprecated
/// tags are left out, and a tag named twice (through an alias) keeps the
/// higher confidence. Names must be normalised.
pub async fn site_tags(
    conn: &mut PgConnection,
    predicted: &[(&str, i16, f32)],
) -> sqlx::Result<Vec<(Tag, f32)>> {
    let names: Vec<&str> = predicted.iter().map(|p| p.0).collect();
    let aliases = crate::tag_relations::aliases_of(&mut *conn, &names).await?;
    let resolved: Vec<(&str, i16, f32)> = predicted
        .iter()
        .map(|&(name, category, confidence)| {
            let name = aliases
                .iter()
                .find(|(antecedent, _)| antecedent == name)
                .map_or(name, |(_, consequent)| consequent.as_str());
            (name, category, confidence)
        })
        .collect();
    let names: Vec<&str> = resolved.iter().map(|r| r.0).collect();
    let mut found = crate::tags::by_names(&mut *conn, &names).await?;
    let missing: Vec<WantedTag<'_>> = resolved
        .iter()
        .filter(|r| !found.iter().any(|t| t.name == r.0))
        .map(|r| WantedTag {
            name: r.0,
            category_id: Some(r.1),
        })
        .collect();
    if !missing.is_empty() {
        found.extend(crate::tags::ensure(&mut *conn, &missing, false).await?);
    }
    let mut tags: Vec<(Tag, f32)> = Vec::new();
    for (name, _, confidence) in resolved {
        let Some(tag) = found.iter().find(|t| t.name == name && !t.is_deprecated) else {
            continue;
        };
        match tags.iter_mut().find(|(t, _)| t.id == tag.id) {
            Some((_, best)) => *best = best.max(confidence),
            None => tags.push((tag.clone(), confidence)),
        }
    }
    Ok(tags)
}

/// A suggested tag.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Suggestion {
    pub tag_id: i32,
    pub name: String,
    pub category_id: i16,
    pub post_count: i32,
    pub confidence: f32,
}

/// The tags suggested for a post, most confident first.
pub async fn for_post(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Vec<Suggestion>> {
    sqlx::query_as(
        "SELECT t.id AS tag_id, t.name, t.category_id, t.post_count, s.confidence
         FROM tag_suggestions s JOIN tags t ON t.id = s.tag_id
         WHERE s.post_id = $1
         ORDER BY s.confidence DESC, t.name",
    )
    .bind(post_id)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use moekura_core::posts::PostStatus;
    use sqlx::PgPool;

    use super::*;
    use crate::posts::{self, NewPost};
    use crate::tags;

    async fn post(pool: &PgPool) -> i64 {
        posts::insert(
            pool,
            NewPost {
                uploader_id: None,
                rating: Rating::General,
                status: PostStatus::Active,
                source: "",
                description: "",
                tag_ids: &[],
            },
        )
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn saves_and_replaces_results(pool: PgPool) {
        let id = post(&pool).await;
        let mut conn = pool.acquire().await.unwrap();
        let wanted = ["cat", "dog"].map(|name| WantedTag {
            name,
            category_id: None,
        });
        let found = tags::ensure(&mut conn, &wanted, false).await.unwrap();
        let (cat, dog) = (found[0].id.min(found[1].id), found[0].id.max(found[1].id));

        let first = NewResult {
            post_id: id,
            model: "m1",
            rating: Rating::Sensitive,
            rating_confidence: 0.6,
            suggestions: &[(cat, 0.5), (dog, 0.9)],
        };
        assert!(save(&mut conn, &first).await.unwrap());
        let names: Vec<(String, f32)> = for_post(&pool, id)
            .await
            .unwrap()
            .into_iter()
            .map(|s| (s.name, s.confidence))
            .collect();
        assert_eq!(names, [("dog".to_owned(), 0.9), ("cat".to_owned(), 0.5)]);

        let second = NewResult {
            model: "m2",
            rating: Rating::General,
            suggestions: &[(cat, 0.7)],
            ..first
        };
        assert!(save(&mut conn, &second).await.unwrap());
        let saved = result(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (saved.model.as_str(), saved.rating),
            ("m2", Rating::General)
        );
        assert_eq!(for_post(&pool, id).await.unwrap().len(), 1);

        // A post deleted meanwhile.
        let gone = NewResult {
            post_id: id + 100,
            ..second
        };
        assert!(!save(&mut conn, &gone).await.unwrap());
        assert!(result(&pool, id + 100).await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn maps_model_names_to_site_tags(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let existing = ["cat", "old_name", "new_name", "retired"].map(|name| WantedTag {
            name,
            category_id: None,
        });
        tags::ensure(&mut conn, &existing, false).await.unwrap();
        sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('alias', 'old_name', 'new_name', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE tags SET is_deprecated = true WHERE name = 'retired'")
            .execute(&pool)
            .await
            .unwrap();

        let predicted = [
            ("cat", 4, 0.9),
            ("old_name", 0, 0.5),
            ("new_name", 0, 0.7),
            ("retired", 0, 0.8),
            ("hatsune_miku", 4, 0.6),
        ];
        let mut found: Vec<(String, i16, f32)> = site_tags(&mut conn, &predicted)
            .await
            .unwrap()
            .into_iter()
            .map(|(t, c)| (t.name, t.category_id, c))
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            found,
            [
                // Existing tags keep their category.
                ("cat".to_owned(), 0, 0.9),
                // New tags take the model's.
                ("hatsune_miku".to_owned(), 4, 0.6),
                ("new_name".to_owned(), 0, 0.7),
            ]
        );
    }
}
