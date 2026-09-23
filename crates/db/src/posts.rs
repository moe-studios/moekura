//! Queries on `posts`.

use sqlx::PgExecutor;
use time::OffsetDateTime;
use uwuu_core::posts::{PostStatus, Rating};

pub struct NewPost<'a> {
    pub uploader_id: Option<i64>,
    pub rating: Rating,
    pub status: PostStatus,
    pub source: &'a str,
    pub description: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub id: i64,
    pub uploader_id: Option<i64>,
    pub rating: Rating,
    pub status: PostStatus,
    pub source: String,
    pub description: String,
    pub parent_id: Option<i64>,
    pub score: i32,
    pub fav_count: i32,
    pub tag_count: i32,
    pub created_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct PostRow {
    id: i64,
    uploader_id: Option<i64>,
    rating: String,
    status: String,
    source: String,
    description: String,
    parent_id: Option<i64>,
    score: i32,
    fav_count: i32,
    tag_count: i32,
    created_at: OffsetDateTime,
}

impl TryFrom<PostRow> for Post {
    type Error = sqlx::Error;

    fn try_from(row: PostRow) -> Result<Self, Self::Error> {
        let bad = |what: &str, value: &str| {
            sqlx::Error::Decode(format!("unknown post {what} `{value}`").into())
        };
        Ok(Post {
            id: row.id,
            uploader_id: row.uploader_id,
            rating: row
                .rating
                .parse()
                .map_err(|()| bad("rating", &row.rating))?,
            status: row
                .status
                .parse()
                .map_err(|()| bad("status", &row.status))?,
            source: row.source,
            description: row.description,
            parent_id: row.parent_id,
            score: row.score,
            fav_count: row.fav_count,
            tag_count: row.tag_count,
            created_at: row.created_at,
        })
    }
}

/// `SELECT <post columns> FROM posts` followed by `$rest`.
macro_rules! select_posts {
    ($rest:literal) => {
        concat!(
            "SELECT id, uploader_id, rating, status, source, description, parent_id, score,
                    fav_count, tag_count, created_at
             FROM posts ",
            $rest
        )
    };
}

pub async fn insert(db: impl PgExecutor<'_>, post: NewPost<'_>) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO posts (uploader_id, rating, status, source, description)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(post.uploader_id)
    .bind(post.rating.code())
    .bind(post.status.as_str())
    .bind(post.source)
    .bind(post.description)
    .fetch_one(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Post>> {
    let row: Option<PostRow> = sqlx::query_as(select_posts!("WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    row.map(Post::try_from).transpose()
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn insert_and_read_back(pool: PgPool) {
        let new = NewPost {
            uploader_id: None,
            rating: Rating::Questionable,
            status: PostStatus::Pending,
            source: "https://example.com/art",
            description: "a description",
        };
        let id = insert(&pool, new).await.unwrap();
        let post = by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (post.rating, post.status),
            (Rating::Questionable, PostStatus::Pending)
        );
        assert_eq!(post.source, "https://example.com/art");
        assert_eq!(post.tag_count, 0);
        assert!(by_id(&pool, id + 1).await.unwrap().is_none());
    }
}
