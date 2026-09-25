//! Pools, their posts in order, and their history (`pools`, `pool_posts`,
//! `pool_versions`).

use moekura_core::search::PoolRef;
use sqlx::{PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;

use crate::posts::Visibility;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Pool {
    pub id: i32,
    pub name: String,
    pub description: String,
    pub category: String,
    pub is_deleted: bool,
    pub version: i32,
    pub updater_name: Option<String>,
    /// All its posts, including those the viewer can't see.
    pub post_count: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// `SELECT <pool columns> FROM pools p LEFT JOIN users u` followed by
/// `$rest`.
macro_rules! select_pools {
    ($rest:literal) => {
        concat!(
            "SELECT p.id, p.name, p.description, p.category, p.is_deleted, p.version,
                    u.name::text AS updater_name,
                    (SELECT count(*) FROM pool_posts pp WHERE pp.pool_id = p.id) AS post_count,
                    p.created_at, p.updated_at
             FROM pools p LEFT JOIN users u ON u.id = p.updater_id ",
            $rest
        )
    };
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<Option<Pool>> {
    sqlx::query_as(select_pools!("WHERE p.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The pool named `name`, regardless of case.
pub async fn by_name(db: impl PgExecutor<'_>, name: &str) -> sqlx::Result<Option<Pool>> {
    sqlx::query_as(select_pools!("WHERE lower(p.name) = lower($1)"))
        .bind(name)
        .fetch_optional(db)
        .await
}

/// The pool a search term names, by id or name.
pub async fn find(db: impl PgExecutor<'_>, pool: &PoolRef) -> sqlx::Result<Option<Pool>> {
    match pool {
        PoolRef::Id(id) => by_id(db, *id).await,
        PoolRef::Name(name) => by_name(db, name).await,
    }
}

/// Which pools [`list`] returns.
#[derive(Debug, Clone, Default)]
pub struct Filter<'a> {
    /// A name prefix, or a pattern with `*` wildcards; empty for all.
    pub name: &'a str,
    pub category: Option<&'a str>,
    pub with_deleted: bool,
}

/// Pools matching `filter`, most recently changed first.
pub async fn list(
    db: impl PgExecutor<'_>,
    filter: &Filter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Pool>> {
    sqlx::query_as(select_pools!(
        "WHERE ($1 = '%' OR lower(p.name) LIKE $1)
           AND ($2::text IS NULL OR p.category = $2)
           AND ($3 OR NOT p.is_deleted)
         ORDER BY p.updated_at DESC, p.id DESC OFFSET $4 LIMIT $5"
    ))
    .bind(crate::tags::like_pattern(&filter.name.to_lowercase()))
    .bind(filter.category)
    .bind(filter.with_deleted)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// A pool's posts in order, all of them.
pub async fn post_ids(db: impl PgExecutor<'_>, pool_id: i32) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar("SELECT post_id FROM pool_posts WHERE pool_id = $1 ORDER BY position")
        .bind(pool_id)
        .fetch_all(db)
        .await
}

/// A pool's posts that `visibility` allows, in order, from `offset`.
pub async fn visible_post_ids(
    db: impl PgExecutor<'_>,
    pool_id: i32,
    visibility: &Visibility,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<i64>> {
    let statuses: Vec<&str> = visibility.statuses.iter().map(|s| s.as_str()).collect();
    sqlx::query_scalar(
        "SELECT pp.post_id FROM pool_posts pp JOIN posts p ON p.id = pp.post_id
         WHERE pp.pool_id = $1
           AND (p.status = ANY($2) OR (p.status = 'pending' AND p.uploader_id = $3))
         ORDER BY pp.position OFFSET $4 LIMIT $5",
    )
    .bind(pool_id)
    .bind(statuses)
    .bind(visibility.viewer)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// How many of a pool's posts `visibility` allows.
pub async fn visible_count(
    db: impl PgExecutor<'_>,
    pool_id: i32,
    visibility: &Visibility,
) -> sqlx::Result<i64> {
    let statuses: Vec<&str> = visibility.statuses.iter().map(|s| s.as_str()).collect();
    sqlx::query_scalar(
        "SELECT count(*) FROM pool_posts pp JOIN posts p ON p.id = pp.post_id
         WHERE pp.pool_id = $1
           AND (p.status = ANY($2) OR (p.status = 'pending' AND p.uploader_id = $3))",
    )
    .bind(pool_id)
    .bind(statuses)
    .bind(visibility.viewer)
    .fetch_one(db)
    .await
}

/// A pool a post is in.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Membership {
    pub id: i32,
    pub name: String,
    pub category: String,
    /// The post's place, 0-based.
    pub position: i32,
    pub post_count: i64,
}

/// The pools (not deleted) post `post_id` is in, series first, then by
/// name.
pub async fn for_post(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Vec<Membership>> {
    sqlx::query_as(
        "SELECT p.id, p.name, p.category, pp.position,
                (SELECT count(*) FROM pool_posts c WHERE c.pool_id = p.id) AS post_count
         FROM pool_posts pp JOIN pools p ON p.id = pp.pool_id
         WHERE pp.post_id = $1 AND NOT p.is_deleted
         ORDER BY p.category DESC, lower(p.name)",
    )
    .bind(post_id)
    .fetch_all(db)
    .await
}

/// The posts next to `position` in a pool that `visibility` allows:
/// (first, previous, next, last).
pub async fn neighbours(
    db: impl PgExecutor<'_>,
    pool_id: i32,
    position: i32,
    visibility: &Visibility,
) -> sqlx::Result<(Option<i64>, Option<i64>, Option<i64>, Option<i64>)> {
    let statuses: Vec<&str> = visibility.statuses.iter().map(|s| s.as_str()).collect();
    sqlx::query_as(
        "WITH visible AS (
             SELECT pp.post_id, pp.position FROM pool_posts pp JOIN posts p ON p.id = pp.post_id
             WHERE pp.pool_id = $1
               AND (p.status = ANY($3) OR (p.status = 'pending' AND p.uploader_id = $4)))
         SELECT (SELECT post_id FROM visible ORDER BY position LIMIT 1),
                (SELECT post_id FROM visible WHERE position < $2 ORDER BY position DESC LIMIT 1),
                (SELECT post_id FROM visible WHERE position > $2 ORDER BY position LIMIT 1),
                (SELECT post_id FROM visible ORDER BY position DESC LIMIT 1)",
    )
    .bind(pool_id)
    .bind(position)
    .bind(statuses)
    .bind(visibility.viewer)
    .fetch_one(db)
    .await
}

/// A pool as saved: what [`save`] writes and each version records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contents {
    pub name: String,
    pub description: String,
    pub category: String,
    pub is_deleted: bool,
    /// In order, without duplicates.
    pub post_ids: Vec<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// Someone else saved the pool since the editor loaded `base`.
    #[error("the pool changed since version {base}; it is now at version {current}")]
    Conflict { base: i32, current: i32 },
    #[error("another pool is already called that")]
    NameTaken,
    /// These ids aren't posts.
    #[error("no such posts: {0:?}")]
    MissingPosts(Vec<i64>),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

fn db_error(e: sqlx::Error) -> SaveError {
    match &e {
        sqlx::Error::Database(db) if db.constraint() == Some("pools_name_idx") => {
            SaveError::NameTaken
        }
        _ => SaveError::Db(e),
    }
}

/// Creates a pool and returns its id.
pub async fn create(
    db: &PgPool,
    contents: &Contents,
    updater_id: Option<i64>,
) -> Result<i32, SaveError> {
    let mut tx = db.begin().await?;
    check_posts(&mut tx, &contents.post_ids).await?;
    let id: i32 = sqlx::query_scalar(
        "INSERT INTO pools (name, description, category, is_deleted, updater_id)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&contents.name)
    .bind(&contents.description)
    .bind(&contents.category)
    .bind(contents.is_deleted)
    .bind(updater_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(db_error)?;
    write_posts(&mut tx, id, &contents.post_ids).await?;
    record_version(&mut tx, id, 1, updater_id, contents).await?;
    tx.commit().await?;
    Ok(id)
}

/// Saves `contents` as pool `id` and returns its version afterwards.
///
/// `base` is the version the editor started from; a different current
/// version is a [`SaveError::Conflict`]. `None` saves over whatever is
/// there. Saving what's already there records no new version.
pub async fn save(
    db: &PgPool,
    id: i32,
    contents: &Contents,
    updater_id: Option<i64>,
    base: Option<i32>,
) -> Result<i32, SaveError> {
    let mut tx = db.begin().await?;
    let (version,): (i32,) = sqlx::query_as("SELECT version FROM pools WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(SaveError::Db(sqlx::Error::RowNotFound))?;
    if let Some(base) = base
        && base != version
    {
        return Err(SaveError::Conflict {
            base,
            current: version,
        });
    }
    if current_contents(&mut tx, id).await? == *contents {
        return Ok(version);
    }
    check_posts(&mut tx, &contents.post_ids).await?;
    sqlx::query(
        "UPDATE pools SET name = $2, description = $3, category = $4, is_deleted = $5,
                          version = $6, updater_id = $7, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&contents.name)
    .bind(&contents.description)
    .bind(&contents.category)
    .bind(contents.is_deleted)
    .bind(version + 1)
    .bind(updater_id)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    write_posts(&mut tx, id, &contents.post_ids).await?;
    record_version(&mut tx, id, version + 1, updater_id, contents).await?;
    tx.commit().await?;
    Ok(version + 1)
}

/// A pool's contents now, as [`save`] compares them.
pub async fn contents(db: &PgPool, id: i32) -> sqlx::Result<Option<Contents>> {
    let mut conn = db.acquire().await?;
    let exists: Option<i32> = sqlx::query_scalar("SELECT id FROM pools WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    match exists {
        Some(_) => Ok(Some(current_contents(&mut conn, id).await?)),
        None => Ok(None),
    }
}

async fn current_contents(conn: &mut PgConnection, id: i32) -> sqlx::Result<Contents> {
    let (name, description, category, is_deleted): (String, String, String, bool) =
        sqlx::query_as("SELECT name, description, category, is_deleted FROM pools WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    Ok(Contents {
        name,
        description,
        category,
        is_deleted,
        post_ids: post_ids(&mut *conn, id).await?,
    })
}

async fn check_posts(conn: &mut PgConnection, ids: &[i64]) -> Result<(), SaveError> {
    let missing: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM unnest($1::bigint[]) AS ids (id)
         WHERE NOT EXISTS (SELECT 1 FROM posts p WHERE p.id = ids.id)",
    )
    .bind(ids)
    .fetch_all(&mut *conn)
    .await?;
    if missing.is_empty() {
        Ok(())
    } else {
        Err(SaveError::MissingPosts(missing))
    }
}

async fn write_posts(conn: &mut PgConnection, id: i32, ids: &[i64]) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM pool_posts WHERE pool_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO pool_posts (pool_id, post_id, position)
         SELECT $1, post_id, (position - 1)::int
         FROM unnest($2::bigint[]) WITH ORDINALITY AS ids (post_id, position)",
    )
    .bind(id)
    .bind(ids)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn record_version(
    conn: &mut PgConnection,
    id: i32,
    version: i32,
    updater_id: Option<i64>,
    contents: &Contents,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO pool_versions
             (pool_id, version, updater_id, name, description, category, is_deleted, post_ids)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(version)
    .bind(updater_id)
    .bind(&contents.name)
    .bind(&contents.description)
    .bind(&contents.category)
    .bind(contents.is_deleted)
    .bind(&contents.post_ids)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Version {
    pub version: i32,
    pub updater_name: Option<String>,
    pub name: String,
    pub description: String,
    pub category: String,
    pub is_deleted: bool,
    pub post_ids: Vec<i64>,
    pub created_at: OffsetDateTime,
}

impl Version {
    pub fn contents(&self) -> Contents {
        Contents {
            name: self.name.clone(),
            description: self.description.clone(),
            category: self.category.clone(),
            is_deleted: self.is_deleted,
            post_ids: self.post_ids.clone(),
        }
    }
}

/// A pool's versions, newest first (at most the latest 500).
pub async fn versions(db: impl PgExecutor<'_>, pool_id: i32) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(
        "SELECT v.version, u.name::text AS updater_name, v.name, v.description, v.category,
                v.is_deleted, v.post_ids, v.created_at
         FROM pool_versions v LEFT JOIN users u ON u.id = v.updater_id
         WHERE v.pool_id = $1 ORDER BY v.version DESC LIMIT 500",
    )
    .bind(pool_id)
    .fetch_all(db)
    .await
}

pub async fn version(
    db: impl PgExecutor<'_>,
    pool_id: i32,
    version: i32,
) -> sqlx::Result<Option<Version>> {
    sqlx::query_as(
        "SELECT v.version, u.name::text AS updater_name, v.name, v.description, v.category,
                v.is_deleted, v.post_ids, v.created_at
         FROM pool_versions v LEFT JOIN users u ON u.id = v.updater_id
         WHERE v.pool_id = $1 AND v.version = $2",
    )
    .bind(pool_id)
    .bind(version)
    .fetch_optional(db)
    .await
}

#[cfg(test)]
mod tests {
    use moekura_core::posts::PostStatus;

    use super::*;

    async fn posts(pool: &PgPool, statuses: &[&str]) -> Vec<i64> {
        let mut ids = Vec::new();
        for status in statuses {
            ids.push(
                sqlx::query_scalar(
                    "INSERT INTO posts (rating, status) VALUES ('g', $1) RETURNING id",
                )
                .bind(status)
                .fetch_one(pool)
                .await
                .unwrap(),
            );
        }
        ids
    }

    fn contents(name: &str, post_ids: &[i64]) -> Contents {
        Contents {
            name: name.into(),
            description: String::new(),
            category: "series".into(),
            is_deleted: false,
            post_ids: post_ids.to_vec(),
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn saves_versions_and_order(pool: PgPool) {
        let ids = posts(&pool, &["active", "active", "pending", "active"]).await;
        let id = create(&pool, &contents("My_Comic", &[ids[1], ids[0]]), None)
            .await
            .unwrap();
        assert_eq!(post_ids(&pool, id).await.unwrap(), [ids[1], ids[0]]);
        assert!(matches!(
            create(&pool, &contents("my_comic", &[]), None).await,
            Err(SaveError::NameTaken)
        ));
        assert!(matches!(
            create(&pool, &contents("Other", &[999_999]), None).await,
            Err(SaveError::MissingPosts(missing)) if missing == [999_999]
        ));

        // Unchanged: no new version.
        let same = contents("My_Comic", &[ids[1], ids[0]]);
        assert_eq!(save(&pool, id, &same, None, Some(1)).await.unwrap(), 1);
        let reordered = contents("My_Comic", &[ids[0], ids[2], ids[1], ids[3]]);
        assert_eq!(save(&pool, id, &reordered, None, Some(1)).await.unwrap(), 2);
        assert!(matches!(
            save(&pool, id, &same, None, Some(1)).await,
            Err(SaveError::Conflict {
                base: 1,
                current: 2
            })
        ));
        assert_eq!(post_ids(&pool, id).await.unwrap(), reordered.post_ids);

        let public = Visibility {
            statuses: vec![PostStatus::Active, PostStatus::Flagged],
            viewer: None,
        };
        assert_eq!(
            visible_post_ids(&pool, id, &public, 0, 10).await.unwrap(),
            [ids[0], ids[1], ids[3]]
        );
        assert_eq!(
            visible_post_ids(&pool, id, &public, 1, 1).await.unwrap(),
            [ids[1]]
        );
        assert_eq!(visible_count(&pool, id, &public).await.unwrap(), 3);
        // Post 1 is third; the pending post before it is skipped.
        assert_eq!(
            neighbours(&pool, id, 2, &public).await.unwrap(),
            (Some(ids[0]), Some(ids[0]), Some(ids[3]), Some(ids[3]))
        );

        let found = by_name(&pool, "MY_COMIC").await.unwrap().unwrap();
        assert_eq!((found.id, found.version, found.post_count), (id, 2, 4));
        let member = for_post(&pool, ids[1]).await.unwrap();
        assert_eq!(member.len(), 1);
        assert_eq!((member[0].position, member[0].post_count), (2, 4));

        let history = versions(&pool, id).await.unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].post_ids, [ids[1], ids[0]]);
        assert_eq!(
            version(&pool, id, 1).await.unwrap().unwrap().contents(),
            same
        );

        // Purging a post takes it out of the pool.
        sqlx::query("DELETE FROM posts WHERE id = $1")
            .bind(ids[3])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(post_ids(&pool, id).await.unwrap().len(), 3);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn lists_by_name_and_category(pool: PgPool) {
        for (name, category, deleted) in [
            ("Cat_Comic", "series", false),
            ("cats", "collection", false),
            ("dogs", "series", true),
        ] {
            let contents = Contents {
                category: category.into(),
                is_deleted: deleted,
                ..contents(name, &[])
            };
            create(&pool, &contents, None).await.unwrap();
        }
        let names = |found: Vec<Pool>| found.into_iter().map(|p| p.name).collect::<Vec<_>>();
        let filter = |name, category, with_deleted| Filter {
            name,
            category,
            with_deleted,
        };
        assert_eq!(
            names(
                list(&pool, &filter("cat", None, false), 0, 10)
                    .await
                    .unwrap()
            ),
            ["cats", "Cat_Comic"]
        );
        assert_eq!(
            names(
                list(&pool, &filter("", Some("series"), true), 0, 10)
                    .await
                    .unwrap()
            ),
            ["dogs", "Cat_Comic"]
        );
        assert_eq!(
            names(
                list(&pool, &filter("*comic", None, false), 0, 10)
                    .await
                    .unwrap()
            ),
            ["Cat_Comic"]
        );
    }
}
