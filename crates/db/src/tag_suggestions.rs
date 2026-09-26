//! Queries on `tagger_results` and `tag_suggestions`: what the tagger
//! made of posts.

use moekura_core::permissions::SystemRole;
use moekura_core::posts::Rating;
use moekura_core::tags::POST_MAX_TAGS;
use sqlx::{PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;

use crate::posts::PostEdit;
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

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error(
        "the account `{0}` has a password, so it isn't used as the tagger's; \
         set tagger.account to another name"
    )]
    HasPassword(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// The id of the tagger's account `name`, created as a member without a
/// password (so nobody can log in as it) if it doesn't exist. An account
/// someone can log in to is refused.
pub async fn tagger_account(db: &PgPool, name: &str) -> Result<i64, AccountError> {
    let find = async || -> sqlx::Result<Option<(i64, bool)>> {
        sqlx::query_as("SELECT id, password_hash IS NOT NULL FROM users WHERE name = $1")
            .bind(name)
            .fetch_optional(db)
            .await
    };
    let found = match find().await? {
        Some(found) => found,
        None => {
            let role = crate::roles::by_system(db, SystemRole::Member).await?;
            let created = crate::users::insert(
                db,
                crate::users::NewUser {
                    name,
                    email: None,
                    password_hash: None,
                    role_id: role.id,
                    status: crate::users::UserStatus::Active,
                },
            )
            .await;
            match created {
                Ok(user) => (user.id, false),
                // Another tagger created it first.
                Err(crate::users::InsertError::NameTaken) => {
                    find().await?.ok_or(sqlx::Error::RowNotFound)?
                }
                Err(crate::users::InsertError::Db(e)) => return Err(e.into()),
                Err(crate::users::InsertError::EmailTaken) => {
                    unreachable!("the account has no email address")
                }
            }
        }
    };
    match found {
        (_, true) => Err(AccountError::HasPassword(name.to_owned())),
        (id, false) => Ok(id),
    }
}

/// Adds `tag_ids` (and the tags they imply) to a post, and sets `rating`
/// if given, credited to `updater_id` in the post's history. Returns
/// whether the post changed; posts that would get too many tags are left
/// alone.
pub async fn apply(
    conn: &mut PgConnection,
    post_id: i64,
    updater_id: i64,
    tag_ids: &[i32],
    rating: Option<Rating>,
) -> sqlx::Result<bool> {
    let Some(post) = crate::posts::lock(&mut *conn, post_id).await? else {
        return Ok(false);
    };
    let mut ids = post.tag_ids.clone();
    ids.extend_from_slice(tag_ids);
    let names: Vec<String> = crate::tags::by_ids(&mut *conn, &ids)
        .await?
        .into_iter()
        .map(|t| t.name)
        .collect();
    let wanted: Vec<WantedTag<'_>> = names
        .iter()
        .map(|name| WantedTag {
            name,
            category_id: None,
        })
        .collect();
    let new_ids: Vec<i32> = crate::tags::for_post(&mut *conn, &wanted, false)
        .await?
        .iter()
        .map(|t| t.id)
        .collect();
    let rating = rating.unwrap_or(post.rating);
    if (new_ids == post.tag_ids && rating == post.rating) || new_ids.len() > POST_MAX_TAGS {
        return Ok(false);
    }
    crate::post_versions::attribute(&mut *conn, Some(updater_id), None).await?;
    crate::posts::update(
        &mut *conn,
        post_id,
        PostEdit {
            rating,
            source: &post.source,
            description: &post.description,
            parent_id: post.parent_id,
            tag_ids: &new_ids,
        },
    )
    .await?;
    Ok(true)
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

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn the_tagger_has_an_account_nobody_can_log_in_to(pool: PgPool) {
        let id = tagger_account(&pool, "tagger").await.unwrap();
        assert_eq!(tagger_account(&pool, "tagger").await.unwrap(), id);
        assert!(!crate::users::has_password(&pool, id).await.unwrap());

        sqlx::query("UPDATE users SET password_hash = 'x' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            tagger_account(&pool, "tagger").await,
            Err(AccountError::HasPassword(_))
        ));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn applies_tags_and_ratings_in_the_taggers_name(pool: PgPool) {
        let id = post(&pool).await;
        let tagger = tagger_account(&pool, "tagger").await.unwrap();
        let mut conn = pool.acquire().await.unwrap();
        let wanted = ["cat", "animal"].map(|name| WantedTag {
            name,
            category_id: None,
        });
        tags::ensure(&mut conn, &wanted, false).await.unwrap();
        sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('implication', 'cat', 'animal', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let cat = tags::by_name(&pool, "cat").await.unwrap().unwrap();

        let mut tx = pool.begin().await.unwrap();
        assert!(
            apply(&mut tx, id, tagger, &[cat.id], Some(Rating::Sensitive))
                .await
                .unwrap()
        );
        tx.commit().await.unwrap();
        let edited = posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(edited.rating, Rating::Sensitive);
        let names: Vec<String> = tags::by_ids(&pool, &edited.tag_ids)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        let latest = crate::post_versions::list(&pool, id).await.unwrap();
        assert_eq!(latest[0].updater_name.as_deref(), Some("tagger"));

        // Nothing new: nothing changes.
        let mut tx = pool.begin().await.unwrap();
        assert!(!apply(&mut tx, id, tagger, &[cat.id], None).await.unwrap());
    }
}
