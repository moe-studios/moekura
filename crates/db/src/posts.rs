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
    /// Sorted and without duplicates.
    pub tag_ids: &'a [i32],
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
    pub tag_ids: Vec<i32>,
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
    tag_ids: Vec<i32>,
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
            tag_ids: row.tag_ids,
            created_at: row.created_at,
        })
    }
}

/// `SELECT <post columns> FROM posts` followed by `$rest`.
macro_rules! select_posts {
    ($rest:literal) => {
        concat!(
            "SELECT id, uploader_id, rating, status, source, description, parent_id, score,
                    fav_count, tag_ids, created_at
             FROM posts ",
            $rest
        )
    };
}

/// Which posts a viewer may see.
#[derive(Debug, Clone, Default)]
pub struct Visibility {
    /// Statuses visible to everyone in this role (active and flagged at
    /// least).
    pub statuses: Vec<PostStatus>,
    /// The viewer, whose own pending uploads are always visible.
    pub viewer: Option<i64>,
}

impl Visibility {
    pub fn allows(&self, post: &Post) -> bool {
        self.statuses.contains(&post.status)
            || (post.status == PostStatus::Pending
                && post.uploader_id.is_some()
                && post.uploader_id == self.viewer)
    }

    fn status_names(&self) -> Vec<&'static str> {
        self.statuses.iter().map(|s| s.as_str()).collect()
    }
}

/// A post as shown in a grid.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Card {
    pub id: i64,
    pub rating: String,
    pub status: String,
    pub media_type: String,
    pub width: i32,
    pub height: i32,
    pub frames: i32,
    pub tag_ids: Vec<i32>,
    /// Storage keys of the 1x and 2x thumbnails, once generated.
    pub thumb: Option<String>,
    pub thumb_2x: Option<String>,
}

/// Newest posts first, optionally only those older than `before` (keyset
/// pagination). `thumb_kinds` names the 1x and 2x thumbnail variants.
pub async fn recent(
    db: impl PgExecutor<'_>,
    visibility: &Visibility,
    before: Option<i64>,
    limit: i64,
    thumb_kinds: (&str, &str),
) -> sqlx::Result<Vec<Card>> {
    sqlx::query_as(
        "SELECT p.id, p.rating, p.status, a.media_type, a.width, a.height, a.frames, p.tag_ids,
                t1.storage_key AS thumb, t2.storage_key AS thumb_2x
         FROM posts p
         JOIN media_assets a ON a.post_id = p.id
         LEFT JOIN media_variants t1 ON t1.asset_id = a.id AND t1.kind = $4
         LEFT JOIN media_variants t2 ON t2.asset_id = a.id AND t2.kind = $5
         WHERE (p.status = ANY($1) OR (p.status = 'pending' AND p.uploader_id = $2))
           AND ($3::bigint IS NULL OR p.id < $3)
         ORDER BY p.id DESC
         LIMIT $6",
    )
    .bind(visibility.status_names())
    .bind(visibility.viewer)
    .bind(before)
    .bind(thumb_kinds.0)
    .bind(thumb_kinds.1)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Grid cards for `ids`, in the same order. Ids without a post (deleted
/// meanwhile) are skipped.
pub async fn cards(
    db: impl PgExecutor<'_>,
    ids: &[i64],
    thumb_kinds: (&str, &str),
) -> sqlx::Result<Vec<Card>> {
    let mut cards: Vec<Card> = sqlx::query_as(
        "SELECT p.id, p.rating, p.status, a.media_type, a.width, a.height, a.frames, p.tag_ids,
                t1.storage_key AS thumb, t2.storage_key AS thumb_2x
         FROM posts p
         JOIN media_assets a ON a.post_id = p.id
         LEFT JOIN media_variants t1 ON t1.asset_id = a.id AND t1.kind = $2
         LEFT JOIN media_variants t2 ON t2.asset_id = a.id AND t2.kind = $3
         WHERE p.id = ANY($1)",
    )
    .bind(ids)
    .bind(thumb_kinds.0)
    .bind(thumb_kinds.1)
    .fetch_all(db)
    .await?;
    cards.sort_by_key(|card| ids.iter().position(|&id| id == card.id));
    Ok(cards)
}

pub async fn insert(db: impl PgExecutor<'_>, post: NewPost<'_>) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO posts (uploader_id, rating, status, source, description, tag_ids)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(post.uploader_id)
    .bind(post.rating.code())
    .bind(post.status.as_str())
    .bind(post.source)
    .bind(post.description)
    .bind(post.tag_ids)
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

    async fn post_with_file(
        pool: &PgPool,
        status: PostStatus,
        uploader: Option<i64>,
        n: u8,
    ) -> i64 {
        let new = NewPost {
            uploader_id: uploader,
            rating: Rating::General,
            status,
            source: "",
            description: "",
            tag_ids: &[],
        };
        let id = insert(pool, new).await.unwrap();
        sqlx::query(
            "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
             VALUES ($1, $2, $3, 'png', 10, 10, 1, 'k')",
        )
        .bind(id)
        .bind(vec![n; 32])
        .bind(vec![n; 16])
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn recent_respects_visibility_and_paginates(pool: PgPool) {
        let uploader: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'up', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let a = post_with_file(&pool, PostStatus::Active, None, 1).await;
        let pending = post_with_file(&pool, PostStatus::Pending, Some(uploader), 2).await;
        let deleted = post_with_file(&pool, PostStatus::Deleted, None, 3).await;
        let b = post_with_file(&pool, PostStatus::Flagged, None, 4).await;
        sqlx::query(
            "INSERT INTO media_variants VALUES ($1, 'thumb-250', 'webp', 10, 10, 1, 'thumb-key')",
        )
        .bind(
            sqlx::query_scalar::<_, i64>("SELECT id FROM media_assets WHERE post_id = $1")
                .bind(b)
                .fetch_one(&pool)
                .await
                .unwrap(),
        )
        .execute(&pool)
        .await
        .unwrap();

        let ids = |cards: Vec<Card>| cards.into_iter().map(|c| c.id).collect::<Vec<_>>();
        let public = Visibility {
            statuses: vec![PostStatus::Active, PostStatus::Flagged],
            viewer: None,
        };
        let kinds = ("thumb-250", "thumb-500");
        let cards = recent(&pool, &public, None, 10, kinds).await.unwrap();
        assert_eq!(cards[0].thumb.as_deref(), Some("thumb-key"));
        assert_eq!(cards[0].thumb_2x, None);
        assert_eq!(ids(cards), [b, a]);

        let own = Visibility {
            viewer: Some(uploader),
            ..public.clone()
        };
        assert_eq!(
            ids(recent(&pool, &own, None, 10, kinds).await.unwrap()),
            [b, pending, a]
        );

        let staff = Visibility {
            statuses: vec![
                PostStatus::Active,
                PostStatus::Flagged,
                PostStatus::Pending,
                PostStatus::Deleted,
            ],
            viewer: None,
        };
        assert_eq!(
            ids(recent(&pool, &staff, None, 2, kinds).await.unwrap()),
            [b, deleted]
        );
        assert_eq!(
            ids(recent(&pool, &staff, Some(deleted), 2, kinds)
                .await
                .unwrap()),
            [pending, a]
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn insert_and_read_back(pool: PgPool) {
        let new = NewPost {
            uploader_id: None,
            rating: Rating::Questionable,
            status: PostStatus::Pending,
            source: "https://example.com/art",
            description: "a description",
            tag_ids: &[],
        };
        let id = insert(&pool, new).await.unwrap();
        let post = by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (post.rating, post.status),
            (Rating::Questionable, PostStatus::Pending)
        );
        assert_eq!(post.source, "https://example.com/art");
        assert!(post.tag_ids.is_empty());
        assert!(by_id(&pool, id + 1).await.unwrap().is_none());
    }
}
