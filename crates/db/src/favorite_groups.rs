//! Favorite groups (`favorite_groups`, `favorite_group_posts`).

use sqlx::{PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;

use crate::posts::Visibility;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Group {
    pub id: i32,
    pub creator_id: i64,
    pub creator_name: String,
    pub name: String,
    pub is_public: bool,
    pub post_count: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

macro_rules! select_groups {
    ($rest:literal) => {
        concat!(
            "SELECT g.id, g.creator_id, u.name::text AS creator_name, g.name, g.is_public,
                    (SELECT count(*) FROM favorite_group_posts gp WHERE gp.group_id = g.id) AS post_count,
                    g.created_at, g.updated_at
             FROM favorite_groups g JOIN users u ON u.id = g.creator_id ",
            $rest
        )
    };
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<Option<Group>> {
    sqlx::query_as(select_groups!("WHERE g.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// `creator_id`'s group named `name`, regardless of case.
pub async fn by_name(
    db: impl PgExecutor<'_>,
    creator_id: i64,
    name: &str,
) -> sqlx::Result<Option<Group>> {
    sqlx::query_as(select_groups!(
        "WHERE g.creator_id = $1 AND lower(g.name) = lower($2)"
    ))
    .bind(creator_id)
    .bind(name)
    .fetch_optional(db)
    .await
}

/// A user's groups by name, private ones only if `with_private`.
pub async fn for_user(
    db: impl PgExecutor<'_>,
    creator_id: i64,
    with_private: bool,
) -> sqlx::Result<Vec<Group>> {
    sqlx::query_as(select_groups!(
        "WHERE g.creator_id = $1 AND ($2 OR g.is_public) ORDER BY lower(g.name)"
    ))
    .bind(creator_id)
    .bind(with_private)
    .fetch_all(db)
    .await
}

/// A group's posts in order, all of them.
pub async fn post_ids(db: impl PgExecutor<'_>, group_id: i32) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT post_id FROM favorite_group_posts WHERE group_id = $1 ORDER BY position",
    )
    .bind(group_id)
    .fetch_all(db)
    .await
}

/// A group's posts that `visibility` allows, in order, from `offset`.
pub async fn visible_post_ids(
    db: impl PgExecutor<'_>,
    group_id: i32,
    visibility: &Visibility,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<i64>> {
    let statuses: Vec<&str> = visibility.statuses.iter().map(|s| s.as_str()).collect();
    sqlx::query_scalar(
        "SELECT gp.post_id FROM favorite_group_posts gp JOIN posts p ON p.id = gp.post_id
         WHERE gp.group_id = $1
           AND (p.status = ANY($2) OR (p.status = 'pending' AND p.uploader_id = $3))
         ORDER BY gp.position OFFSET $4 LIMIT $5",
    )
    .bind(group_id)
    .bind(statuses)
    .bind(visibility.viewer)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The ids of `creator_id`'s groups that contain `post_id`.
pub async fn containing(
    db: impl PgExecutor<'_>,
    creator_id: i64,
    post_id: i64,
) -> sqlx::Result<Vec<i32>> {
    sqlx::query_scalar(
        "SELECT g.id FROM favorite_groups g JOIN favorite_group_posts gp ON gp.group_id = g.id
         WHERE g.creator_id = $1 AND gp.post_id = $2",
    )
    .bind(creator_id)
    .bind(post_id)
    .fetch_all(db)
    .await
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("you already have a group called that")]
    NameTaken,
    #[error("no such posts: {0:?}")]
    MissingPosts(Vec<i64>),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

fn db_error(e: sqlx::Error) -> SaveError {
    match &e {
        sqlx::Error::Database(db) if db.constraint() == Some("favorite_groups_name_idx") => {
            SaveError::NameTaken
        }
        _ => SaveError::Db(e),
    }
}

/// What a group holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contents {
    pub name: String,
    pub is_public: bool,
    /// In order, without duplicates.
    pub post_ids: Vec<i64>,
}

pub async fn create(db: &PgPool, creator_id: i64, contents: &Contents) -> Result<i32, SaveError> {
    let mut tx = db.begin().await?;
    check_posts(&mut tx, &contents.post_ids).await?;
    let id: i32 = sqlx::query_scalar(
        "INSERT INTO favorite_groups (creator_id, name, is_public) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(creator_id)
    .bind(&contents.name)
    .bind(contents.is_public)
    .fetch_one(&mut *tx)
    .await
    .map_err(db_error)?;
    write_posts(&mut tx, id, &contents.post_ids).await?;
    tx.commit().await?;
    Ok(id)
}

/// Replaces group `id`'s name, visibility and posts.
pub async fn save(db: &PgPool, id: i32, contents: &Contents) -> Result<(), SaveError> {
    let mut tx = db.begin().await?;
    check_posts(&mut tx, &contents.post_ids).await?;
    sqlx::query(
        "UPDATE favorite_groups SET name = $2, is_public = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&contents.name)
    .bind(contents.is_public)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    write_posts(&mut tx, id, &contents.post_ids).await?;
    tx.commit().await?;
    Ok(())
}

/// Adds a post to the end of group `id`; false if it was already there.
pub async fn append(db: &PgPool, id: i32, post_id: i64) -> sqlx::Result<bool> {
    let mut tx = db.begin().await?;
    // Serialises appends to the same group.
    sqlx::query("SELECT 1 FROM favorite_groups WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let added = sqlx::query(
        "INSERT INTO favorite_group_posts (group_id, post_id, position)
         SELECT $1, $2, coalesce(max(position) + 1, 0) FROM favorite_group_posts WHERE group_id = $1
         ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(post_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        == 1;
    sqlx::query("UPDATE favorite_groups SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(added)
}

/// Takes a post out of group `id`; false if it wasn't there.
pub async fn remove_post(db: impl PgExecutor<'_>, id: i32, post_id: i64) -> sqlx::Result<bool> {
    let result =
        sqlx::query("DELETE FROM favorite_group_posts WHERE group_id = $1 AND post_id = $2")
            .bind(id)
            .bind(post_id)
            .execute(db)
            .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn delete(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM favorite_groups WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn count_for_user(db: impl PgExecutor<'_>, creator_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM favorite_groups WHERE creator_id = $1")
        .bind(creator_id)
        .fetch_one(db)
        .await
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
    sqlx::query("DELETE FROM favorite_group_posts WHERE group_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO favorite_group_posts (group_id, post_id, position)
         SELECT $1, post_id, (position - 1)::int
         FROM unnest($2::bigint[]) WITH ORDINALITY AS ids (post_id, position)",
    )
    .bind(id)
    .bind(ids)
    .execute(&mut *conn)
    .await?;
    Ok(())
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

    async fn post(pool: &PgPool) -> i64 {
        sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn groups(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let (a, b) = (post(&pool).await, post(&pool).await);
        let contents = |name: &str, is_public, post_ids: Vec<i64>| Contents {
            name: name.into(),
            is_public,
            post_ids,
        };
        let id = create(&pool, alice, &contents("Best", true, vec![b]))
            .await
            .unwrap();
        create(&pool, alice, &contents("secret", false, vec![]))
            .await
            .unwrap();
        assert!(matches!(
            create(&pool, alice, &contents("BEST", true, vec![])).await,
            Err(SaveError::NameTaken)
        ));
        // Another user may use the name.
        create(&pool, bob, &contents("best", true, vec![]))
            .await
            .unwrap();

        assert!(append(&pool, id, a).await.unwrap());
        assert!(!append(&pool, id, a).await.unwrap());
        assert_eq!(post_ids(&pool, id).await.unwrap(), [b, a]);
        assert_eq!(containing(&pool, alice, a).await.unwrap(), [id]);
        save(&pool, id, &contents("Best", true, vec![a, b]))
            .await
            .unwrap();
        assert_eq!(post_ids(&pool, id).await.unwrap(), [a, b]);
        assert!(remove_post(&pool, id, a).await.unwrap());

        let found = by_name(&pool, alice, "best").await.unwrap().unwrap();
        assert_eq!(
            (found.id, found.post_count, found.creator_name.as_str()),
            (id, 1, "alice")
        );
        assert_eq!(for_user(&pool, alice, false).await.unwrap().len(), 1);
        assert_eq!(for_user(&pool, alice, true).await.unwrap().len(), 2);
        assert_eq!(count_for_user(&pool, alice).await.unwrap(), 2);
        delete(&pool, id).await.unwrap();
        assert!(by_id(&pool, id).await.unwrap().is_none());
    }
}
