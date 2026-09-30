//! Comments on posts, their votes and reports. Triggers keep
//! `posts.comment_count` and `posts.last_commented_at` in step with the
//! visible comments, and `comments.score` with the votes.

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
    pub score: i32,
    pub created_at: OffsetDateTime,
    pub edited_at: Option<OffsetDateTime>,
    /// Pinned by staff to the top of its post's comments.
    pub is_sticky: bool,
    /// Posted without bumping its post in `order:comment_bumped`.
    pub do_not_bump: bool,
}

/// `SELECT <comment columns> FROM comments c LEFT JOIN users u` followed
/// by `$rest`.
macro_rules! select_comments {
    ($rest:literal) => {
        concat!(
            "SELECT c.id, c.post_id, c.creator_id, u.name::text AS creator_name, c.body,
                    c.is_deleted, c.score, c.created_at, c.edited_at, c.is_sticky, c.do_not_bump
             FROM comments c LEFT JOIN users u ON u.id = c.creator_id ",
            $rest
        )
    };
}

/// Adds a comment and returns its id. Unless `bump`, the post keeps its
/// place in `order:comment_bumped`.
pub async fn create(
    db: impl PgExecutor<'_>,
    post_id: i64,
    creator_id: i64,
    body: &str,
    bump: bool,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO comments (post_id, creator_id, body, do_not_bump)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(post_id)
    .bind(creator_id)
    .bind(body)
    .bind(!bump)
    .fetch_one(db)
    .await
}

/// Pins or unpins a comment; false if it already was.
pub async fn set_sticky(db: impl PgExecutor<'_>, id: i64, sticky: bool) -> sqlx::Result<bool> {
    let result =
        sqlx::query("UPDATE comments SET is_sticky = $2 WHERE id = $1 AND is_sticky <> $2")
            .bind(id)
            .bind(sticky)
            .execute(db)
            .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<Comment>> {
    sqlx::query_as(select_comments!("WHERE c.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The latest `limit` comments on a post, sticky ones first, then oldest
/// first, with deleted ones if `with_deleted`.
pub async fn for_post(
    db: impl PgExecutor<'_>,
    post_id: i64,
    with_deleted: bool,
    limit: i64,
) -> sqlx::Result<Vec<Comment>> {
    let mut comments: Vec<Comment> = sqlx::query_as(select_comments!(
        "WHERE c.post_id = $1 AND ($2 OR NOT c.is_deleted)
         ORDER BY c.is_sticky DESC, c.id DESC LIMIT $3"
    ))
    .bind(post_id)
    .bind(with_deleted)
    .bind(limit)
    .fetch_all(db)
    .await?;
    comments.reverse();
    // Stable, so each part stays oldest first.
    comments.sort_by_key(|c| !c.is_sticky);
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
           AND p.rating = ANY($8)
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
    .bind(visibility.rating_codes())
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

/// Sets `user_id`'s vote on a comment: `1`, `-1`, or `0` to take it back.
pub async fn vote(
    db: impl PgExecutor<'_>,
    user_id: i64,
    comment_id: i64,
    score: i16,
) -> sqlx::Result<()> {
    if score == 0 {
        sqlx::query("DELETE FROM comment_votes WHERE user_id = $1 AND comment_id = $2")
            .bind(user_id)
            .bind(comment_id)
            .execute(db)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO comment_votes (user_id, comment_id, score) VALUES ($1, $2, $3)
             ON CONFLICT (comment_id, user_id) DO UPDATE SET score = EXCLUDED.score
             WHERE comment_votes.score <> EXCLUDED.score",
        )
        .bind(user_id)
        .bind(comment_id)
        .bind(score)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// `user_id`'s votes among `comment_ids`, as (comment, score) pairs.
pub async fn votes_of(
    db: impl PgExecutor<'_>,
    user_id: i64,
    comment_ids: &[i64],
) -> sqlx::Result<Vec<(i64, i16)>> {
    sqlx::query_as(
        "SELECT comment_id, score FROM comment_votes WHERE user_id = $1 AND comment_id = ANY($2)",
    )
    .bind(user_id)
    .bind(comment_ids)
    .fetch_all(db)
    .await
}

/// How many comments a user has written that aren't deleted.
pub async fn count_by_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM comments WHERE creator_id = $1 AND NOT is_deleted")
        .bind(user_id)
        .fetch_one(db)
        .await
}

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("You already reported this comment.")]
    AlreadyReported,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Reports a comment to the moderators.
pub async fn report(
    db: impl PgExecutor<'_>,
    comment_id: i64,
    creator_id: i64,
    reason: &str,
) -> Result<(), ReportError> {
    sqlx::query("INSERT INTO comment_reports (comment_id, creator_id, reason) VALUES ($1, $2, $3)")
        .bind(comment_id)
        .bind(creator_id)
        .bind(reason)
        .execute(db)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => ReportError::AlreadyReported,
            _ => ReportError::Db(e),
        })?;
    Ok(())
}

/// Closes a comment's open reports as `dismissed` or `upheld`; returns how
/// many there were.
pub async fn resolve_reports(
    db: impl PgExecutor<'_>,
    comment_id: i64,
    upheld: bool,
    resolver_id: Option<i64>,
) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE comment_reports SET status = $2, resolver_id = $3, resolved_at = now()
         WHERE comment_id = $1 AND status = 'open'",
    )
    .bind(comment_id)
    .bind(if upheld { "upheld" } else { "dismissed" })
    .bind(resolver_id)
    .execute(db)
    .await?;
    Ok(result.rows_affected())
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Report {
    pub id: i64,
    pub comment_id: i64,
    pub creator_name: Option<String>,
    pub reason: String,
    pub created_at: OffsetDateTime,
}

/// Open reports on up to `limit` comments, the comment reported longest
/// ago first, each comment's reports together and oldest first. Pages go
/// by the id of a comment's oldest open report, as [`crate::flags::open`].
pub async fn open_reports(
    db: impl PgExecutor<'_>,
    after: i64,
    limit: i64,
) -> sqlx::Result<Vec<Report>> {
    sqlx::query_as(
        "WITH queued AS (
             SELECT comment_id, min(id) AS first FROM comment_reports WHERE status = 'open'
             GROUP BY comment_id HAVING min(id) > $1 ORDER BY first LIMIT $2)
         SELECT r.id, r.comment_id, u.name::text AS creator_name, r.reason, r.created_at
         FROM queued q
         JOIN comment_reports r ON r.comment_id = q.comment_id AND r.status = 'open'
         LEFT JOIN users u ON u.id = r.creator_id
         ORDER BY q.first, r.id",
    )
    .bind(after)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Comments by id, in the order of `ids`; missing ones are left out.
pub async fn by_ids(db: impl PgExecutor<'_>, ids: &[i64]) -> sqlx::Result<Vec<Comment>> {
    let mut comments: Vec<Comment> = sqlx::query_as(select_comments!("WHERE c.id = ANY($1)"))
        .bind(ids)
        .fetch_all(db)
        .await?;
    comments.sort_by_key(|c| ids.iter().position(|&id| id == c.id));
    Ok(comments)
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

        let first = create(&pool, post, alice, "First", true).await.unwrap();
        let second = create(&pool, post, alice, "Second", true).await.unwrap();
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

    async fn bumped_at(pool: &PgPool, post: i64) -> Option<OffsetDateTime> {
        sqlx::query_scalar("SELECT last_comment_bumped_at FROM posts WHERE id = $1")
            .bind(post)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn only_bumping_comments_bump(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let post = post(&pool, "active").await;
        let first = create(&pool, post, alice, "First", true).await.unwrap();
        let first_at = by_id(&pool, first).await.unwrap().unwrap().created_at;
        let quiet = create(&pool, post, alice, "Quiet", false).await.unwrap();
        let quiet_at = by_id(&pool, quiet).await.unwrap().unwrap().created_at;
        assert_eq!(bumped_at(&pool, post).await, Some(first_at));
        assert_eq!(counts(&pool, post).await, (2, Some(quiet_at)));
        assert!(by_id(&pool, quiet).await.unwrap().unwrap().do_not_bump);
        set_deleted(&pool, first, true).await.unwrap();
        assert_eq!(bumped_at(&pool, post).await, None);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn sticky_comments_come_first(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let post = post(&pool, "active").await;
        let mut ids = Vec::new();
        for body in ["one", "two", "three", "four"] {
            ids.push(create(&pool, post, alice, body, true).await.unwrap());
        }
        assert!(set_sticky(&pool, ids[0], true).await.unwrap());
        assert!(!set_sticky(&pool, ids[0], true).await.unwrap());
        let bodies = |comments: Vec<Comment>| -> Vec<String> {
            comments.into_iter().map(|c| c.body).collect()
        };
        // Even when older than the latest `limit`.
        assert_eq!(
            bodies(for_post(&pool, post, false, 3).await.unwrap()),
            ["one", "three", "four"]
        );
        set_sticky(&pool, ids[0], false).await.unwrap();
        assert_eq!(
            bodies(for_post(&pool, post, false, 3).await.unwrap()),
            ["two", "three", "four"]
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn votes_and_reports(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let post = post(&pool, "active").await;
        let id = create(&pool, post, alice, "Hello", true).await.unwrap();
        let score = |pool: PgPool| async move { by_id(&pool, id).await.unwrap().unwrap().score };

        vote(&pool, alice, id, 1).await.unwrap();
        vote(&pool, bob, id, 1).await.unwrap();
        assert_eq!(score(pool.clone()).await, 2);
        vote(&pool, bob, id, -1).await.unwrap();
        vote(&pool, bob, id, -1).await.unwrap();
        assert_eq!(score(pool.clone()).await, 0);
        vote(&pool, alice, id, 0).await.unwrap();
        assert_eq!(score(pool.clone()).await, -1);
        assert_eq!(votes_of(&pool, bob, &[id]).await.unwrap(), [(id, -1)]);
        assert!(votes_of(&pool, alice, &[id]).await.unwrap().is_empty());

        report(&pool, id, bob, "rude").await.unwrap();
        assert!(matches!(
            report(&pool, id, bob, "again").await,
            Err(ReportError::AlreadyReported)
        ));
        report(&pool, id, alice, "mine").await.unwrap();
        let open = open_reports(&pool, 0, 10).await.unwrap();
        assert_eq!(
            open.iter().map(|r| r.reason.as_str()).collect::<Vec<_>>(),
            ["rude", "mine"]
        );
        assert_eq!(
            resolve_reports(&pool, id, false, Some(alice))
                .await
                .unwrap(),
            2
        );
        assert!(open_reports(&pool, 0, 10).await.unwrap().is_empty());
        // Settled reports don't stop a new one.
        report(&pool, id, bob, "still rude").await.unwrap();
        assert_eq!(count_by_user(&pool, alice).await.unwrap(), 1);
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
            ids.push(create(&pool, post, user, body, true).await.unwrap());
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
            hidden_tags: Vec::new(),
            statuses: vec![PostStatus::Active, PostStatus::Flagged],
            viewer: None,
            ratings: Vec::new(),
            deleted_by_default: false,
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
            hidden_tags: Vec::new(),
            viewer: None,
            ratings: Vec::new(),
            deleted_by_default: false,
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
