//! Comments on posts. A trigger keeps `posts.comment_count` and
//! `posts.last_commented_at` in step with the visible ones.

use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::posts::Visibility;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Comment {
    pub id: i64,
    pub post_id: i64,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub body: String,
    pub is_deleted: bool,
    pub created_at: OffsetDateTime,
    pub edited_at: Option<OffsetDateTime>,
}

/// `SELECT <comment columns> FROM comments c LEFT JOIN users u` followed
/// by `$rest`.
macro_rules! select_comments {
    ($rest:literal) => {
        concat!(
            "SELECT c.id, c.post_id, c.creator_id, u.name::text AS creator_name, c.body,
                    c.is_deleted, c.created_at, c.edited_at
             FROM comments c LEFT JOIN users u ON u.id = c.creator_id ",
            $rest
        )
    };
}

/// Adds a comment and returns its id.
pub async fn create(
    db: impl PgExecutor<'_>,
    post_id: i64,
    creator_id: i64,
    body: &str,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO comments (post_id, creator_id, body) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(post_id)
    .bind(creator_id)
    .bind(body)
    .fetch_one(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Comment>> {
    sqlx::query_as(select_comments!("WHERE c.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The latest `limit` comments on a post, oldest first, with deleted ones
/// if `with_deleted`.
pub async fn for_post(
    db: impl PgExecutor<'_>,
    post_id: i64,
    with_deleted: bool,
    limit: i64,
) -> sqlx::Result<Vec<Comment>> {
    let mut comments: Vec<Comment> = sqlx::query_as(select_comments!(
        "WHERE c.post_id = $1 AND ($2 OR NOT c.is_deleted) ORDER BY c.id DESC LIMIT $3"
    ))
    .bind(post_id)
    .bind(with_deleted)
    .bind(limit)
    .fetch_all(db)
    .await?;
    comments.reverse();
    Ok(comments)
}

/// Which comments [`list`] returns.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub post_id: Option<i64>,
    pub creator_id: Option<i64>,
    pub with_deleted: bool,
}

/// Comments matching `filter` on posts `visibility` allows, newest first,
/// older than comment `before` if given.
pub async fn list(
    db: impl PgExecutor<'_>,
    visibility: &Visibility,
    filter: &Filter,
    before: Option<i64>,
    limit: i64,
) -> sqlx::Result<Vec<Comment>> {
    let statuses: Vec<&str> = visibility.statuses.iter().map(|s| s.as_str()).collect();
    sqlx::query_as(select_comments!(
        "JOIN posts p ON p.id = c.post_id
         WHERE (p.status = ANY($1) OR (p.status = 'pending' AND p.uploader_id = $2))
           AND ($3 OR NOT c.is_deleted)
           AND ($4::bigint IS NULL OR c.post_id = $4)
           AND ($5::bigint IS NULL OR c.creator_id = $5)
           AND ($6::bigint IS NULL OR c.id < $6)
         ORDER BY c.id DESC LIMIT $7"
    ))
    .bind(statuses)
    .bind(visibility.viewer)
    .bind(filter.with_deleted)
    .bind(filter.post_id)
    .bind(filter.creator_id)
    .bind(before)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Replaces a comment's text, marking it edited.
pub async fn update(db: impl PgExecutor<'_>, id: i64, body: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE comments SET body = $2, edited_at = now() WHERE id = $1 AND body <> $2")
        .bind(id)
        .bind(body)
        .execute(db)
        .await?;
    Ok(())
}

/// Deletes or restores a comment; false if it already was.
pub async fn set_deleted(db: impl PgExecutor<'_>, id: i64, deleted: bool) -> sqlx::Result<bool> {
    let result =
        sqlx::query("UPDATE comments SET is_deleted = $2 WHERE id = $1 AND is_deleted <> $2")
            .bind(id)
            .bind(deleted)
            .execute(db)
            .await?;
    Ok(result.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use moekura_core::posts::PostStatus;
    use sqlx::PgPool;

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

    async fn post(pool: &PgPool, status: &str) -> i64 {
        sqlx::query_scalar("INSERT INTO posts (rating, status) VALUES ('g', $1) RETURNING id")
            .bind(status)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn counts(pool: &PgPool, post: i64) -> (i32, Option<OffsetDateTime>) {
        sqlx::query_as("SELECT comment_count, last_commented_at FROM posts WHERE id = $1")
            .bind(post)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn counts_follow_visible_comments(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let post = post(&pool, "active").await;
        assert_eq!(counts(&pool, post).await, (0, None));

        let first = create(&pool, post, alice, "First").await.unwrap();
        let second = create(&pool, post, alice, "Second").await.unwrap();
        let second_at = by_id(&pool, second).await.unwrap().unwrap().created_at;
        assert_eq!(counts(&pool, post).await, (2, Some(second_at)));

        assert!(set_deleted(&pool, second, true).await.unwrap());
        assert!(!set_deleted(&pool, second, true).await.unwrap());
        let first_at = by_id(&pool, first).await.unwrap().unwrap().created_at;
        assert_eq!(counts(&pool, post).await, (1, Some(first_at)));
        set_deleted(&pool, first, true).await.unwrap();
        assert_eq!(counts(&pool, post).await, (0, None));
        set_deleted(&pool, second, false).await.unwrap();
        assert_eq!(counts(&pool, post).await, (1, Some(second_at)));
        sqlx::query("DELETE FROM comments WHERE id = $1")
            .bind(second)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, post).await, (0, None));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn lists_and_edits(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let active = post(&pool, "active").await;
        let pending = post(&pool, "pending").await;
        let mut ids = Vec::new();
        for (post, user, body) in [
            (active, alice, "one"),
            (active, bob, "two"),
            (pending, bob, "hidden"),
            (active, alice, "three"),
        ] {
            ids.push(create(&pool, post, user, body).await.unwrap());
        }
        set_deleted(&pool, ids[1], true).await.unwrap();

        let bodies = |found: Vec<Comment>| found.into_iter().map(|c| c.body).collect::<Vec<_>>();
        assert_eq!(
            bodies(for_post(&pool, active, false, 10).await.unwrap()),
            ["one", "three"]
        );
        assert_eq!(
            bodies(for_post(&pool, active, true, 2).await.unwrap()),
            ["two", "three"]
        );

        let public = Visibility {
            statuses: vec![PostStatus::Active, PostStatus::Flagged],
            viewer: None,
        };
        assert_eq!(
            bodies(
                list(&pool, &public, &Filter::default(), None, 10)
                    .await
                    .unwrap()
            ),
            ["three", "one"]
        );
        let staff = Filter {
            with_deleted: true,
            ..Filter::default()
        };
        assert_eq!(
            bodies(
                list(&pool, &public, &staff, Some(ids[3]), 10)
                    .await
                    .unwrap()
            ),
            ["two", "one"]
        );
        let by_bob = Filter {
            creator_id: Some(bob),
            with_deleted: true,
            ..Filter::default()
        };
        let uploader = Visibility {
            viewer: None,
            statuses: vec![PostStatus::Active, PostStatus::Pending],
        };
        assert_eq!(
            bodies(list(&pool, &uploader, &by_bob, None, 10).await.unwrap()),
            ["hidden", "two"]
        );

        update(&pool, ids[0], "one, edited").await.unwrap();
        let edited = by_id(&pool, ids[0]).await.unwrap().unwrap();
        assert_eq!(edited.body, "one, edited");
        assert!(edited.edited_at.is_some());
        assert_eq!(edited.creator_name.as_deref(), Some("alice"));
    }
}
