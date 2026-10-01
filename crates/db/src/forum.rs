//! The forum: categories, topics, posts, votes and read tracking
//! (`forum_categories`, `forum_topics`, `forum_posts`, `forum_post_votes`,
//! `forum_topic_visits`, `forum_read_marks`).

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Category {
    pub id: i16,
    pub name: String,
    pub description: String,
}

pub async fn categories(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<Category>> {
    sqlx::query_as("SELECT id, name, description FROM forum_categories ORDER BY position, id")
        .fetch_all(db)
        .await
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Topic {
    pub id: i64,
    pub category_id: i16,
    pub category_name: String,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub title: String,
    pub is_sticky: bool,
    pub is_locked: bool,
    pub is_deleted: bool,
    pub merged_into_id: Option<i64>,
    pub post_count: i32,
    pub last_posted_at: OffsetDateTime,
    pub last_poster_name: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Posted in since the viewer last read it (false without a viewer).
    pub unread: bool,
}

/// `SELECT <topic columns> …` for viewer `$1` followed by `$rest`.
macro_rules! select_topics {
    ($rest:literal) => {
        concat!(
            "SELECT t.id, t.category_id, c.name AS category_name, t.creator_id,
                    cu.name::text AS creator_name, t.title, t.is_sticky, t.is_locked,
                    t.is_deleted, t.merged_into_id, t.post_count, t.last_posted_at,
                    lu.name::text AS last_poster_name, t.created_at, t.updated_at,
                    ($1::bigint IS NOT NULL AND t.last_posted_at > greatest(
                        (SELECT v.read_at FROM forum_topic_visits v
                         WHERE v.user_id = $1 AND v.topic_id = t.id),
                        (SELECT m.read_at FROM forum_read_marks m WHERE m.user_id = $1),
                        '-infinity'::timestamptz)) AS unread
             FROM forum_topics t
             JOIN forum_categories c ON c.id = t.category_id
             LEFT JOIN users cu ON cu.id = t.creator_id
             LEFT JOIN users lu ON lu.id = t.last_poster_id ",
            $rest
        )
    };
}

pub async fn topic(
    db: impl PgExecutor<'_>,
    viewer: Option<i64>,
    id: i64,
) -> sqlx::Result<Option<Topic>> {
    sqlx::query_as(select_topics!("WHERE t.id = $2"))
        .bind(viewer)
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Which topics [`topics`] returns.
#[derive(Debug, Clone, Default)]
pub struct TopicFilter<'a> {
    pub category_id: Option<i16>,
    /// Words in the title (any case); empty for all.
    pub title: &'a str,
    pub with_deleted: bool,
    pub creator_id: Option<i64>,
    pub ids: Option<&'a [i64]>,
}

/// Topics matching `filter`, stickied first (when not searching), then
/// the latest posted in.
pub async fn topics(
    db: impl PgExecutor<'_>,
    viewer: Option<i64>,
    filter: &TopicFilter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Topic>> {
    let pattern = format!(
        "%{}%",
        crate::tags::like_pattern(filter.title.trim()).trim_end_matches('%')
    );
    sqlx::query_as(select_topics!(
        "WHERE ($2::smallint IS NULL OR t.category_id = $2)
           AND ($3 = '' OR t.title ILIKE $4)
           AND ($5 OR NOT t.is_deleted)
           AND ($6::bigint IS NULL OR t.creator_id = $6)
           AND ($7::bigint[] IS NULL OR t.id = ANY($7))
         ORDER BY ($3 = '' AND t.is_sticky) DESC, t.last_posted_at DESC, t.id DESC
         OFFSET $8 LIMIT $9"
    ))
    .bind(viewer)
    .bind(filter.category_id)
    .bind(filter.title.trim())
    .bind(pattern)
    .bind(filter.with_deleted)
    .bind(filter.creator_id)
    .bind(filter.ids)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Post {
    pub id: i64,
    pub topic_id: i64,
    pub topic_title: String,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub updater_name: Option<String>,
    pub body: String,
    pub is_hidden: bool,
    pub score: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// `SELECT <post columns> FROM forum_posts p …` followed by `$rest`.
macro_rules! select_posts {
    ($rest:literal) => {
        concat!(
            "SELECT p.id, p.topic_id, t.title AS topic_title, p.creator_id,
                    cu.name::text AS creator_name, uu.name::text AS updater_name, p.body,
                    p.is_hidden, p.score, p.created_at, p.updated_at
             FROM forum_posts p
             JOIN forum_topics t ON t.id = p.topic_id
             LEFT JOIN users cu ON cu.id = p.creator_id
             LEFT JOIN users uu ON uu.id = p.updater_id ",
            $rest
        )
    };
}

pub async fn post(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Post>> {
    sqlx::query_as(select_posts!("WHERE p.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Which posts [`posts`] returns.
#[derive(Debug, Clone, Default)]
pub struct PostFilter<'a> {
    pub topic_id: Option<i64>,
    pub creator_id: Option<i64>,
    /// Words in the text.
    pub words: &'a str,
    pub with_hidden: bool,
    /// Posts in deleted topics too.
    pub with_deleted_topics: bool,
    /// Newest first even within a topic.
    pub newest_first: bool,
}

/// Posts matching `filter`: a topic's in order (unless `newest_first`),
/// others newest first.
pub async fn posts(
    db: impl PgExecutor<'_>,
    filter: &PostFilter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Post>> {
    sqlx::query_as(select_posts!(
        "WHERE ($1::bigint IS NULL OR p.topic_id = $1)
           AND ($2::bigint IS NULL OR p.creator_id = $2)
           AND ($3 = '' OR (to_tsvector('simple', p.body) @@ plainto_tsquery('simple', $3)
                            AND NOT p.is_hidden))
           AND ($4 OR NOT p.is_hidden)
           AND ($5 OR NOT t.is_deleted)
         ORDER BY CASE WHEN $1::bigint IS NULL OR $8 THEN -p.id ELSE p.id END
         OFFSET $6 LIMIT $7"
    ))
    .bind(filter.topic_id)
    .bind(filter.creator_id)
    .bind(filter.words.trim())
    .bind(filter.with_hidden)
    .bind(filter.with_deleted_topics)
    .bind(offset)
    .bind(limit)
    .bind(filter.newest_first)
    .fetch_all(db)
    .await
}

/// How many posts of a topic come before post `id`.
pub async fn position(
    db: impl PgExecutor<'_>,
    topic_id: i64,
    id: i64,
    with_hidden: bool,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM forum_posts WHERE topic_id = $1 AND id < $2 AND ($3 OR NOT is_hidden)",
    )
    .bind(topic_id)
    .bind(id)
    .bind(with_hidden)
    .fetch_one(db)
    .await
}

/// Starts a topic with its first post; returns both ids.
pub async fn create_topic(
    db: &PgPool,
    category_id: i16,
    creator_id: Option<i64>,
    title: &str,
    body: &str,
) -> sqlx::Result<(i64, i64)> {
    let mut tx = db.begin().await?;
    let topic: i64 = sqlx::query_scalar(
        "INSERT INTO forum_topics (category_id, creator_id, title) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(category_id)
    .bind(creator_id)
    .bind(title)
    .fetch_one(&mut *tx)
    .await?;
    let post = create_post(&mut *tx, topic, creator_id, body).await?;
    tx.commit().await?;
    Ok((topic, post))
}

pub async fn create_post(
    db: impl PgExecutor<'_>,
    topic_id: i64,
    creator_id: Option<i64>,
    body: &str,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO forum_posts (topic_id, creator_id, body) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(topic_id)
    .bind(creator_id)
    .bind(body)
    .fetch_one(db)
    .await
}

pub async fn update_post(
    db: impl PgExecutor<'_>,
    id: i64,
    body: &str,
    updater_id: Option<i64>,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE forum_posts SET body = $2, updater_id = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(body)
    .bind(updater_id)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

pub async fn set_post_hidden(db: impl PgExecutor<'_>, id: i64, hidden: bool) -> sqlx::Result<bool> {
    let done = sqlx::query("UPDATE forum_posts SET is_hidden = $2 WHERE id = $1")
        .bind(id)
        .bind(hidden)
        .execute(db)
        .await?;
    Ok(done.rows_affected() > 0)
}

/// Changes a topic's title and category.
pub async fn update_topic(
    db: impl PgExecutor<'_>,
    id: i64,
    title: &str,
    category_id: i16,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE forum_topics SET title = $2, category_id = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(title)
    .bind(category_id)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// What a moderator changes about a topic; `None` leaves it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    pub sticky: Option<bool>,
    pub locked: Option<bool>,
    pub deleted: Option<bool>,
}

pub async fn set_flags(db: impl PgExecutor<'_>, id: i64, flags: Flags) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE forum_topics SET is_sticky = coalesce($2, is_sticky),
                                 is_locked = coalesce($3, is_locked),
                                 is_deleted = coalesce($4, is_deleted), updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(flags.sticky)
    .bind(flags.locked)
    .bind(flags.deleted)
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Moves topic `from`'s posts into topic `into`, then deletes `from`,
/// remembering where they went.
pub async fn merge(db: &PgPool, from: i64, into: i64) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE forum_posts SET topic_id = $2 WHERE topic_id = $1")
        .bind(from)
        .bind(into)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE forum_topics SET is_deleted = true, merged_into_id = $2, updated_at = now()
         WHERE id = $1",
    )
    .bind(from)
    .bind(into)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Sets `user_id`'s vote on a post: `1`, `-1`, or `0` to take it back.
pub async fn vote(
    db: impl PgExecutor<'_>,
    post_id: i64,
    user_id: i64,
    score: i16,
) -> sqlx::Result<()> {
    let sql = if score == 0 {
        "DELETE FROM forum_post_votes WHERE post_id = $1 AND user_id = $2 AND $3 = 0"
    } else {
        "INSERT INTO forum_post_votes (post_id, user_id, score) VALUES ($1, $2, $3)
         ON CONFLICT (post_id, user_id) DO UPDATE SET score = EXCLUDED.score
         WHERE forum_post_votes.score <> EXCLUDED.score"
    };
    sqlx::query(sql)
        .bind(post_id)
        .bind(user_id)
        .bind(score)
        .execute(db)
        .await?;
    Ok(())
}

/// `user_id`'s votes on posts `post_ids`.
pub async fn votes_of(
    db: impl PgExecutor<'_>,
    user_id: i64,
    post_ids: &[i64],
) -> sqlx::Result<Vec<(i64, i16)>> {
    sqlx::query_as(
        "SELECT post_id, score FROM forum_post_votes WHERE user_id = $1 AND post_id = ANY($2)",
    )
    .bind(user_id)
    .bind(post_ids)
    .fetch_all(db)
    .await
}

/// A vote, for the Danbooru API.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Vote {
    pub post_id: i64,
    pub user_id: i64,
    pub score: i16,
    pub created_at: OffsetDateTime,
}

pub async fn votes(
    db: impl PgExecutor<'_>,
    user_id: i64,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Vote>> {
    sqlx::query_as(
        "SELECT post_id, user_id, score, created_at FROM forum_post_votes WHERE user_id = $1
         ORDER BY created_at DESC OFFSET $2 LIMIT $3",
    )
    .bind(user_id)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Notes that `user_id` has read topic `topic_id` up to now.
pub async fn visit(db: impl PgExecutor<'_>, user_id: i64, topic_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO forum_topic_visits (user_id, topic_id) VALUES ($1, $2)
         ON CONFLICT (user_id, topic_id) DO UPDATE SET read_at = now()",
    )
    .bind(user_id)
    .bind(topic_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Marks every topic read for `user_id`.
pub async fn mark_all_read(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO forum_read_marks (user_id) VALUES ($1)
         ON CONFLICT (user_id) DO UPDATE SET read_at = now()",
    )
    .bind(user_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Links a tag relation (alias or implication) or a bulk update request
/// to its topic.
pub async fn link_request(
    db: impl PgExecutor<'_>,
    relation_id: Option<i32>,
    request_id: Option<i32>,
    topic_id: i64,
) -> sqlx::Result<()> {
    let (sql, id) = match (relation_id, request_id) {
        (Some(id), _) => (
            "UPDATE tag_relations SET forum_topic_id = $2 WHERE id = $1",
            id,
        ),
        (None, Some(id)) => (
            "UPDATE bulk_update_requests SET forum_topic_id = $2 WHERE id = $1",
            id,
        ),
        (None, None) => return Ok(()),
    };
    sqlx::query(sql).bind(id).bind(topic_id).execute(db).await?;
    Ok(())
}

/// The forum topic of a tag relation or bulk update request, if any.
pub async fn request_topic(
    db: impl PgExecutor<'_>,
    relation_id: Option<i32>,
    request_id: Option<i32>,
) -> sqlx::Result<Option<i64>> {
    let found: Option<Option<i64>> = match (relation_id, request_id) {
        (Some(id), _) => {
            sqlx::query_scalar("SELECT forum_topic_id FROM tag_relations WHERE id = $1")
                .bind(id)
                .fetch_optional(db)
                .await?
        }
        (None, Some(id)) => {
            sqlx::query_scalar("SELECT forum_topic_id FROM bulk_update_requests WHERE id = $1")
                .bind(id)
                .fetch_optional(db)
                .await?
        }
        (None, None) => None,
    };
    Ok(found.flatten())
}

/// The tag relation or bulk update request a topic is about.
pub async fn topic_request(db: &PgPool, topic_id: i64) -> sqlx::Result<(Option<i32>, Option<i32>)> {
    let relation: Option<i32> =
        sqlx::query_scalar("SELECT id FROM tag_relations WHERE forum_topic_id = $1 LIMIT 1")
            .bind(topic_id)
            .fetch_optional(db)
            .await?;
    let request: Option<i32> =
        sqlx::query_scalar("SELECT id FROM bulk_update_requests WHERE forum_topic_id = $1 LIMIT 1")
            .bind(topic_id)
            .fetch_optional(db)
            .await?;
    Ok((relation, request))
}

/// Everyone who has a visible post in topic `topic_id`.
pub async fn participants(db: impl PgExecutor<'_>, topic_id: i64) -> sqlx::Result<Vec<i64>> {
    sqlx::query_scalar(
        "SELECT DISTINCT creator_id FROM forum_posts
         WHERE topic_id = $1 AND creator_id IS NOT NULL AND NOT is_hidden",
    )
    .bind(topic_id)
    .fetch_all(db)
    .await
}

pub async fn count_by_creator(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM forum_posts WHERE creator_id = $1 AND NOT is_hidden")
        .bind(user_id)
        .fetch_one(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id)
             VALUES ($1, (SELECT id FROM roles WHERE system_key = 'member')) RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn topics_posts_votes_and_reading(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let general = categories(&pool).await.unwrap()[0].id;
        let (topic, first) = create_topic(&pool, general, Some(alice), "Hello", "First post")
            .await
            .unwrap();
        let found = super::topic(&pool, Some(bob), topic)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.post_count, 1);
        assert!(found.unread);
        visit(&pool, bob, topic).await.unwrap();
        assert!(
            !super::topic(&pool, Some(bob), topic)
                .await
                .unwrap()
                .unwrap()
                .unread
        );
        let reply = create_post(&pool, topic, Some(bob), "A reply about cats")
            .await
            .unwrap();
        let found = super::topic(&pool, Some(alice), topic)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.post_count, 2);
        assert_eq!(found.last_poster_name.as_deref(), Some("bob"));

        vote(&pool, first, bob, 1).await.unwrap();
        vote(&pool, first, alice, -1).await.unwrap();
        vote(&pool, first, alice, 1).await.unwrap();
        assert_eq!(post(&pool, first).await.unwrap().unwrap().score, 2);
        assert_eq!(
            votes_of(&pool, bob, &[first, reply]).await.unwrap(),
            [(first, 1)]
        );

        let found = posts(
            &pool,
            &PostFilter {
                words: "cats",
                ..PostFilter::default()
            },
            0,
            10,
        )
        .await
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, reply);
        set_post_hidden(&pool, reply, true).await.unwrap();
        assert_eq!(
            super::topic(&pool, None, topic)
                .await
                .unwrap()
                .unwrap()
                .post_count,
            1
        );

        let (other, _) = create_topic(&pool, general, Some(bob), "Hello again", "Dup")
            .await
            .unwrap();
        set_flags(
            &pool,
            topic,
            Flags {
                sticky: Some(true),
                ..Flags::default()
            },
        )
        .await
        .unwrap();
        let listed = topics(&pool, None, &TopicFilter::default(), 0, 10)
            .await
            .unwrap();
        assert_eq!(listed[0].id, topic, "stickied first");
        let searched = topics(
            &pool,
            None,
            &TopicFilter {
                title: "again",
                ..TopicFilter::default()
            },
            0,
            10,
        )
        .await
        .unwrap();
        assert_eq!(searched.len(), 1);
        merge(&pool, other, topic).await.unwrap();
        assert_eq!(
            super::topic(&pool, None, topic)
                .await
                .unwrap()
                .unwrap()
                .post_count,
            2
        );
        let merged = super::topic(&pool, None, other).await.unwrap().unwrap();
        assert!(merged.is_deleted && merged.merged_into_id == Some(topic));
        mark_all_read(&pool, alice).await.unwrap();
        assert!(
            !super::topic(&pool, Some(alice), topic)
                .await
                .unwrap()
                .unwrap()
                .unread
        );
    }
}
