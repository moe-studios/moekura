//! Queries on `posts`.

use moekura_core::posts::{PostLock, PostStatus, Rating};
use sqlx::PgExecutor;
use time::OffsetDateTime;

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
    /// Comments that aren't deleted.
    pub comment_count: i32,
    pub last_commented_at: Option<OffsetDateTime>,
    /// The last comment that bumped the post (was posted without "don't
    /// bump").
    pub last_comment_bumped_at: Option<OffsetDateTime>,
    /// Notes that aren't deleted.
    pub note_count: i32,
    pub last_noted_at: Option<OffsetDateTime>,
    pub tag_ids: Vec<i32>,
    pub created_at: OffsetDateTime,
    /// What staff locked against changes.
    pub locks: Vec<PostLock>,
    /// The Pixiv work the source links to.
    pub pixiv_id: Option<i64>,
    /// Its notes are drawn on the picture with their text.
    pub has_embedded_notes: bool,
}

impl Post {
    pub fn is_locked(&self, lock: PostLock) -> bool {
        self.locks.contains(&lock)
    }
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
    comment_count: i32,
    last_commented_at: Option<OffsetDateTime>,
    last_comment_bumped_at: Option<OffsetDateTime>,
    note_count: i32,
    last_noted_at: Option<OffsetDateTime>,
    tag_ids: Vec<i32>,
    created_at: OffsetDateTime,
    locks: Vec<String>,
    pixiv_id: Option<i64>,
    has_embedded_notes: bool,
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
            comment_count: row.comment_count,
            last_commented_at: row.last_commented_at,
            last_comment_bumped_at: row.last_comment_bumped_at,
            note_count: row.note_count,
            last_noted_at: row.last_noted_at,
            tag_ids: row.tag_ids,
            created_at: row.created_at,
            locks: row
                .locks
                .iter()
                .map(|l| PostLock::parse(l).ok_or_else(|| bad("lock", l)))
                .collect::<Result<_, _>>()?,
            pixiv_id: row.pixiv_id,
            has_embedded_notes: row.has_embedded_notes,
        })
    }
}

/// `SELECT <post columns> FROM posts` followed by `$rest`.
macro_rules! select_posts {
    ($rest:literal) => {
        concat!(
            "SELECT id, uploader_id, rating, status, source, description, parent_id, score,
                    fav_count, comment_count, last_commented_at, last_comment_bumped_at,
                    note_count, last_noted_at, tag_ids, created_at, locks, pixiv_id,
                    has_embedded_notes
             FROM posts ",
            $rest
        )
    };
}

/// Sets whether post `id`'s notes are drawn on the picture.
pub async fn set_embedded_notes(
    db: impl PgExecutor<'_>,
    id: i64,
    embedded: bool,
) -> sqlx::Result<bool> {
    let done = sqlx::query("UPDATE posts SET has_embedded_notes = $2 WHERE id = $1")
        .bind(id)
        .bind(embedded)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Which posts a viewer may see.
#[derive(Debug, Clone, Default)]
pub struct Visibility {
    /// Statuses visible to everyone in this role (active and flagged at
    /// least).
    pub statuses: Vec<PostStatus>,
    /// The viewer, whose own pending uploads are always visible.
    pub viewer: Option<i64>,
    /// The only ratings visible (the site's for visitors); empty for all.
    pub ratings: Vec<Rating>,
    /// Searches without a `status:` filter include deleted posts, if
    /// `statuses` has them: the viewer's choice.
    pub deleted_by_default: bool,
    /// Posts with any of these tags are hidden (banned artists'), sorted.
    pub hidden_tags: Vec<i32>,
}

impl Visibility {
    /// Whether the viewer reviews uploads (and so sees pending posts):
    /// staff, who may also see who flagged what.
    pub fn reviews_posts(&self) -> bool {
        self.statuses.contains(&PostStatus::Pending)
    }

    pub fn allows(&self, post: &Post) -> bool {
        let status = self.statuses.contains(&post.status)
            || (post.status == PostStatus::Pending
                && post.uploader_id.is_some()
                && post.uploader_id == self.viewer);
        status
            && self.allows_rating(post.rating)
            && !post
                .tag_ids
                .iter()
                .any(|t| self.hidden_tags.binary_search(t).is_ok())
    }

    pub fn allows_rating(&self, rating: Rating) -> bool {
        self.ratings.is_empty() || self.ratings.contains(&rating)
    }

    /// The codes of the ratings visible, for `p.rating = ANY(…)`: all four
    /// when nothing is left out.
    pub fn rating_codes(&self) -> Vec<&'static str> {
        Rating::ALL
            .into_iter()
            .filter(|r| self.allows_rating(*r))
            .map(Rating::code)
            .collect()
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
    /// The thumbnails are square crops.
    pub square: bool,
}

/// Grid cards for `ids`, in the same order. Ids without a post (deleted
/// meanwhile) are skipped.
pub async fn cards(
    db: impl PgExecutor<'_>,
    ids: &[i64],
    thumb_kinds: (&str, &str),
) -> sqlx::Result<Vec<Card>> {
    // Square (crop-<size>) thumbnails fall back to the usual ones for
    // files processed before they existed.
    let mut cards: Vec<Card> = sqlx::query_as(
        "SELECT p.id, p.rating, p.status, a.media_type, a.width, a.height, a.frames, p.tag_ids,
                coalesce(t1.storage_key, f1.storage_key) AS thumb,
                coalesce(t2.storage_key, f2.storage_key) AS thumb_2x,
                t1.storage_key IS NOT NULL AND $2 LIKE 'crop-%' AS square
         FROM posts p
         JOIN media_assets a ON a.post_id = p.id
         LEFT JOIN media_variants t1 ON t1.asset_id = a.id AND t1.kind = $2
         LEFT JOIN media_variants t2 ON t2.asset_id = a.id AND t2.kind = $3
         LEFT JOIN media_variants f1 ON f1.asset_id = a.id AND f1.kind = replace($2, 'crop-', 'thumb-')
         LEFT JOIN media_variants f2 ON f2.asset_id = a.id AND f2.kind = replace($3, 'crop-', 'thumb-')
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

/// The posts among `ids` that exist, in no particular order.
pub async fn by_ids(db: impl PgExecutor<'_>, ids: &[i64]) -> sqlx::Result<Vec<Post>> {
    let rows: Vec<PostRow> = sqlx::query_as(select_posts!("WHERE id = ANY($1)"))
        .bind(ids)
        .fetch_all(db)
        .await?;
    rows.into_iter().map(Post::try_from).collect()
}

/// Moves a post from one of `from` to `to`; false if it wasn't in one of
/// them (so concurrent moderators can't both act).
pub async fn set_status(
    db: impl PgExecutor<'_>,
    id: i64,
    from: &[PostStatus],
    to: PostStatus,
) -> sqlx::Result<bool> {
    let from: Vec<&str> = from.iter().map(|s| s.as_str()).collect();
    let result = sqlx::query(
        "UPDATE posts SET status = $3, updated_at = now() WHERE id = $1 AND status = ANY($2)",
    )
    .bind(id)
    .bind(from)
    .bind(to.as_str())
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Records who approved a post.
pub async fn set_approver(
    db: impl PgExecutor<'_>,
    id: i64,
    approver_id: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE posts SET approver_id = $2 WHERE id = $1")
        .bind(id)
        .bind(approver_id)
        .execute(db)
        .await?;
    Ok(())
}

/// Deletes a post row and everything hanging off it (media records,
/// versions, favorites, votes). Files are the caller's business.
pub async fn delete(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<bool> {
    let result = sqlx::query("DELETE FROM posts WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// Posts with `status`, oldest first, after `after` (keyset), with their
/// uploaders' names.
/// Vote totals and children of a post, for APIs that report them.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Extras {
    pub post_id: i64,
    pub up_votes: i64,
    pub down_votes: i64,
    /// Any child post, deleted ones included.
    pub has_children: bool,
    /// A child that's active or flagged.
    pub has_active_children: bool,
}

/// [`Extras`] for each of `ids`.
pub async fn extras(db: impl PgExecutor<'_>, ids: &[i64]) -> sqlx::Result<Vec<Extras>> {
    sqlx::query_as(
        "SELECT p.id AS post_id,
                (SELECT count(*) FROM post_votes v WHERE v.post_id = p.id AND v.score > 0) AS up_votes,
                (SELECT count(*) FROM post_votes v WHERE v.post_id = p.id AND v.score < 0) AS down_votes,
                EXISTS (SELECT 1 FROM posts c WHERE c.parent_id = p.id) AS has_children,
                EXISTS (SELECT 1 FROM posts c WHERE c.parent_id = p.id
                        AND c.status IN ('active', 'flagged')) AS has_active_children
         FROM unnest($1::bigint[]) AS p (id)",
    )
    .bind(ids)
    .fetch_all(db)
    .await
}

pub async fn by_status(
    db: impl PgExecutor<'_>,
    status: PostStatus,
    after: i64,
    limit: i64,
) -> sqlx::Result<Vec<(i64, Option<String>)>> {
    sqlx::query_as(
        "SELECT p.id, u.name::text FROM posts p LEFT JOIN users u ON u.id = p.uploader_id
         WHERE p.status = $1 AND p.id > $2 ORDER BY p.id LIMIT $3",
    )
    .bind(status.as_str())
    .bind(after)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// How many posts a user uploaded, leaving out deleted ones.
pub async fn count_by_uploader(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM posts WHERE uploader_id = $1 AND status <> 'deleted'")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// A user's uploads as upload limits count them.
pub async fn upload_counts(
    db: impl PgExecutor<'_>,
    user_id: i64,
) -> sqlx::Result<moekura_core::uploads::UploadCounts> {
    let (pending, today, approved, deleted): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'pending'),
                count(*) FILTER (WHERE created_at > now() - interval '1 day'),
                count(*) FILTER (WHERE status IN ('active', 'flagged')),
                count(*) FILTER (WHERE status = 'deleted')
         FROM posts WHERE uploader_id = $1",
    )
    .bind(user_id)
    .fetch_one(db)
    .await?;
    Ok(moekura_core::uploads::UploadCounts {
        pending,
        today,
        approved,
        deleted,
    })
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

/// Replaces a post's locks (in the order of [`PostLock::ALL`]).
pub async fn set_locks(db: impl PgExecutor<'_>, id: i64, locks: &[PostLock]) -> sqlx::Result<()> {
    let names: Vec<&str> = PostLock::ALL
        .iter()
        .filter(|l| locks.contains(l))
        .map(|l| l.as_str())
        .collect();
    sqlx::query("UPDATE posts SET locks = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(names)
        .execute(db)
        .await?;
    Ok(())
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
           AND rating = ANY($4)
         ORDER BY id LIMIT 100",
    )
    .bind(root)
    .bind(visibility.status_names())
    .bind(visibility.viewer)
    .bind(visibility.rating_codes())
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
            hidden_tags: Vec::new(),
            statuses: vec![PostStatus::Active],
            viewer: None,
            ratings: Vec::new(),
            deleted_by_default: false,
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
