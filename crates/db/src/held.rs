//! Writing held for review as likely spam: comments, forum posts and
//! messages, kept hidden with why until the staff approve or reject them.

use sqlx::{PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Comment,
    ForumPost,
    Dmail,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Comment => "comment",
            Kind::ForumPost => "forum_post",
            Kind::Dmail => "dmail",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Kind::Comment, Kind::ForumPost, Kind::Dmail]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Held {
    pub kind: String,
    pub id: i64,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub body: String,
    /// The forum topic's title, or the message's.
    pub title: Option<String>,
    pub reason: String,
    pub created_at: OffsetDateTime,
    /// The comment's post, or the forum post's topic.
    pub parent_id: Option<i64>,
    /// The message's recipient.
    pub recipient_id: Option<i64>,
    pub recipient_name: Option<String>,
}

/// Everything held, as one list: `$1` limits it, `$2` offsets it, and
/// `$3`, if not null, picks one `kind` and `$4` one id.
const HELD: &str = "
    SELECT * FROM (
        SELECT 'comment' AS kind, c.id, c.creator_id, u.name::text AS creator_name, c.body,
               NULL::text AS title, c.held_reason AS reason, c.created_at,
               c.post_id AS parent_id, NULL::bigint AS recipient_id, NULL::text AS recipient_name
        FROM comments c LEFT JOIN users u ON u.id = c.creator_id
        WHERE c.held_reason IS NOT NULL
        UNION ALL
        SELECT 'forum_post', p.id, p.creator_id, u.name::text, p.body, t.title, p.held_reason,
               p.created_at, p.topic_id, NULL, NULL
        FROM forum_posts p JOIN forum_topics t ON t.id = p.topic_id
        LEFT JOIN users u ON u.id = p.creator_id
        WHERE p.held_reason IS NOT NULL
        UNION ALL
        SELECT 'dmail', d.id, d.from_id, u.name::text, d.body, d.title, d.held_reason,
               d.created_at, NULL, d.to_id, r.name::text
        FROM dmails d LEFT JOIN users u ON u.id = d.from_id LEFT JOIN users r ON r.id = d.to_id
        WHERE d.held_reason IS NOT NULL
    ) held
    WHERE $3::text IS NULL OR (kind = $3 AND id = $4)
    ORDER BY created_at, id
    LIMIT $1 OFFSET $2";

/// What's held, oldest first.
pub async fn list(db: impl PgExecutor<'_>, offset: i64, limit: i64) -> sqlx::Result<Vec<Held>> {
    sqlx::query_as(HELD)
        .bind(limit)
        .bind(offset)
        .bind(None::<&str>)
        .bind(0_i64)
        .fetch_all(db)
        .await
}

/// One held item.
pub async fn get(db: impl PgExecutor<'_>, kind: Kind, id: i64) -> sqlx::Result<Option<Held>> {
    sqlx::query_as(HELD)
        .bind(1_i64)
        .bind(0_i64)
        .bind(kind.as_str())
        .bind(id)
        .fetch_optional(db)
        .await
}

pub async fn count(db: impl PgExecutor<'_>) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM comments WHERE held_reason IS NOT NULL)
              + (SELECT count(*) FROM forum_posts WHERE held_reason IS NOT NULL)
              + (SELECT count(*) FROM dmails WHERE held_reason IS NOT NULL)",
    )
    .fetch_one(db)
    .await
}

/// Lets a held item through, as if it had just been posted; false if it
/// wasn't held (any more).
pub async fn approve(db: &PgPool, kind: Kind, id: i64) -> sqlx::Result<bool> {
    let mut tx = db.begin().await?;
    let done = match kind {
        Kind::Comment => {
            sqlx::query(
                "UPDATE comments SET is_deleted = false, held_reason = NULL
                 WHERE id = $1 AND held_reason IS NOT NULL",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?
        }
        Kind::ForumPost => {
            let done = sqlx::query(
                "UPDATE forum_posts SET is_hidden = false, held_reason = NULL
                 WHERE id = $1 AND held_reason IS NOT NULL",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE forum_topics SET is_deleted = false, is_held = false
                 WHERE is_held AND id = (SELECT topic_id FROM forum_posts WHERE id = $1)",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
            done
        }
        Kind::Dmail => {
            sqlx::query(
                "UPDATE dmails SET is_deleted = false, held_reason = NULL
                 WHERE id = $1 AND held_reason IS NOT NULL",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?
        }
    };
    tx.commit().await?;
    Ok(done.rows_affected() == 1)
}

/// Keeps a held item hidden for good (a message never reaches its
/// recipient); false if it wasn't held (any more).
pub async fn reject(db: impl PgExecutor<'_>, kind: Kind, id: i64) -> sqlx::Result<bool> {
    let sql = match kind {
        Kind::Comment => {
            "UPDATE comments SET held_reason = NULL WHERE id = $1 AND held_reason IS NOT NULL"
        }
        Kind::ForumPost => {
            "UPDATE forum_posts SET held_reason = NULL WHERE id = $1 AND held_reason IS NOT NULL"
        }
        Kind::Dmail => "DELETE FROM dmails WHERE id = $1 AND held_reason IS NOT NULL",
    };
    let done = sqlx::query(sql).bind(id).execute(db).await?;
    Ok(done.rows_affected() == 1)
}

/// How many times `user_id` posted exactly `body` (comments, forum posts
/// and messages sent) in the past day.
pub async fn repeats(db: impl PgExecutor<'_>, user_id: i64, body: &str) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM comments
                 WHERE creator_id = $1 AND body = $2 AND created_at > now() - interval '1 day')
              + (SELECT count(*) FROM forum_posts
                 WHERE creator_id = $1 AND body = $2 AND created_at > now() - interval '1 day')
              + (SELECT count(*) FROM dmails
                 WHERE owner_id = $1 AND from_id = $1 AND body = $2
                   AND created_at > now() - interval '1 day')",
    )
    .bind(user_id)
    .bind(body)
    .fetch_one(db)
    .await
}
