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

/// Like [`by_id`], locking the row until the transaction ends so edits
/// don't overwrite each other.
pub async fn lock(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Post>> {
    let row: Option<PostRow> = sqlx::query_as(select_posts!("WHERE id = $1 FOR UPDATE"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    row.map(Post::try_from).transpose()
}

/// The editable fields of a post.
pub struct PostEdit<'a> {
    pub rating: Rating,
    pub source: &'a str,
    pub description: &'a str,
    pub parent_id: Option<i64>,
    /// Sorted and without duplicates.
    pub tag_ids: &'a [i32],
}

pub async fn update(db: impl PgExecutor<'_>, id: i64, edit: PostEdit<'_>) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE posts SET rating = $2, source = $3, description = $4, parent_id = $5,
                          tag_ids = $6, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(edit.rating.code())
    .bind(edit.source)
    .bind(edit.description)
    .bind(edit.parent_id)
    .bind(edit.tag_ids)
    .execute(db)
    .await?;
    Ok(())
}

/// Whether `ancestor` is `post` or one of its parents, grandparents, …
/// Setting `post` as `ancestor`'s parent would then make a loop.
pub async fn has_ancestor(db: impl PgExecutor<'_>, post: i64, ancestor: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "WITH RECURSIVE up (id, parent_id, depth) AS (
             SELECT id, parent_id, 0 FROM posts WHERE id = $1
             UNION ALL
             SELECT p.id, p.parent_id, up.depth + 1
             FROM posts p JOIN up ON p.id = up.parent_id
             WHERE up.depth < 1000
         )
         SELECT EXISTS (SELECT 1 FROM up WHERE id = $2)",
    )
    .bind(post)
    .bind(ancestor)
    .fetch_one(db)
    .await
}

/// A post's family as the viewer may see it: `root` and its children,
/// oldest first.
pub async fn family(
    db: impl PgExecutor<'_>,
    root: i64,
    visibility: &Visibility,
) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT id FROM posts
         WHERE (id = $1 OR parent_id = $1)
           AND (status = ANY($2) OR (status = 'pending' AND uploader_id = $3))
         ORDER BY id LIMIT 100",
    )
    .bind(root)
    .bind(visibility.status_names())
    .bind(visibility.viewer)
    .fetch_all(db)
    .await
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
    async fn cards_come_back_in_the_order_asked(pool: PgPool) {
        let a = post_with_file(&pool, PostStatus::Active, None, 1).await;
        let b = post_with_file(&pool, PostStatus::Active, None, 2).await;
        let asset: i64 = sqlx::query_scalar("SELECT id FROM media_assets WHERE post_id = $1")
            .bind(b)
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO media_variants VALUES ($1, 'thumb-250', 'webp', 10, 10, 1, 'thumb-key')",
        )
        .bind(asset)
        .execute(&pool)
        .await
        .unwrap();
        let kinds = ("thumb-250", "thumb-500");
        let found = cards(&pool, &[b, a + 100, a], kinds).await.unwrap();
        let ids: Vec<i64> = found.iter().map(|c| c.id).collect();
        assert_eq!(ids, [b, a]);
        assert_eq!(found[0].thumb.as_deref(), Some("thumb-key"));
        assert_eq!(found[0].thumb_2x, None);
        assert_eq!(found[1].thumb, None);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn edits_and_families(pool: PgPool) {
        let parent = post_with_file(&pool, PostStatus::Active, None, 1).await;
        let child = post_with_file(&pool, PostStatus::Active, None, 2).await;
        let hidden = post_with_file(&pool, PostStatus::Deleted, None, 3).await;
        for id in [child, hidden] {
            let mut tx = pool.begin().await.unwrap();
            let post = lock(&mut *tx, id).await.unwrap().unwrap();
            update(
                &mut *tx,
                id,
                PostEdit {
                    rating: Rating::Explicit,
                    source: "s",
                    description: &post.description,
                    parent_id: Some(parent),
                    tag_ids: &[],
                },
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
        let edited = by_id(&pool, child).await.unwrap().unwrap();
        assert_eq!(
            (edited.rating, edited.source.as_str(), edited.parent_id),
            (Rating::Explicit, "s", Some(parent))
        );
        let public = Visibility {
            statuses: vec![PostStatus::Active],
            viewer: None,
        };
        assert_eq!(
            family(&pool, parent, &public).await.unwrap(),
            [parent, child]
        );
        assert!(has_ancestor(&pool, child, parent).await.unwrap());
        assert!(has_ancestor(&pool, child, child).await.unwrap());
        assert!(!has_ancestor(&pool, parent, child).await.unwrap());
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
