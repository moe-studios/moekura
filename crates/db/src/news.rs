//! Site news (`news_updates`): announcements admins post, shown at the
//! top of every page. Every change tells the site cache, which keeps the
//! current news.

use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct NewsUpdate {
    pub id: i64,
    pub body: String,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub expires_at: Option<OffsetDateTime>,
    pub is_deleted: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl NewsUpdate {
    /// Whether it's shown at `now`.
    pub fn is_current(&self, now: OffsetDateTime) -> bool {
        !self.is_deleted && self.expires_at.is_none_or(|at| at > now)
    }
}

/// `SELECT <news columns> FROM news_updates n …` followed by `$rest`.
macro_rules! select_news {
    ($rest:literal) => {
        concat!(
            "SELECT n.id, n.body, n.creator_id, c.name::text AS creator_name, n.expires_at,
                    n.is_deleted, n.created_at, n.updated_at
             FROM news_updates n LEFT JOIN users c ON c.id = n.creator_id ",
            $rest
        )
    };
}

/// News that's shown now, newest first; the site cache keeps a few, so
/// older news shows once newer news expires.
pub async fn current(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<NewsUpdate>> {
    sqlx::query_as(select_news!(
        "WHERE NOT n.is_deleted AND (n.expires_at IS NULL OR n.expires_at > now())
         ORDER BY n.id DESC LIMIT 5"
    ))
    .fetch_all(db)
    .await
}

/// All news, deleted included, newest first.
pub async fn list(
    db: impl PgExecutor<'_>,
    with_deleted: bool,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<NewsUpdate>> {
    sqlx::query_as(select_news!(
        "WHERE $1 OR NOT n.is_deleted ORDER BY n.id DESC LIMIT $2 OFFSET $3"
    ))
    .bind(with_deleted)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<NewsUpdate>> {
    sqlx::query_as(select_news!("WHERE n.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// Tells every node's site cache that the news changed, once the caller's
/// transaction commits.
async fn notify(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_notify($1, 'news')")
        .bind(crate::site_cache::CHANNEL)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub async fn create(
    conn: &mut PgConnection,
    creator_id: Option<i64>,
    body: &str,
    expires_at: Option<OffsetDateTime>,
) -> sqlx::Result<i64> {
    let id = sqlx::query_scalar(
        "INSERT INTO news_updates (creator_id, body, expires_at) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(creator_id)
    .bind(body)
    .bind(expires_at)
    .fetch_one(&mut *conn)
    .await?;
    notify(conn).await?;
    Ok(id)
}

/// False when there's no such news.
pub async fn update(
    conn: &mut PgConnection,
    id: i64,
    body: &str,
    expires_at: Option<OffsetDateTime>,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE news_updates SET body = $2, expires_at = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(body)
    .bind(expires_at)
    .execute(&mut *conn)
    .await?;
    notify(conn).await?;
    Ok(result.rows_affected() == 1)
}

/// False when there's no such news, or it already was (or wasn't) deleted.
pub async fn set_deleted(conn: &mut PgConnection, id: i64, deleted: bool) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE news_updates SET is_deleted = $2, updated_at = now()
         WHERE id = $1 AND is_deleted <> $2",
    )
    .bind(id)
    .bind(deleted)
    .execute(&mut *conn)
    .await?;
    notify(conn).await?;
    Ok(result.rows_affected() == 1)
}

/// Remembers that `user_id` dismissed news up to `id`, in their settings
/// (as `dismissed_news`), so it stays dismissed wherever they log in.
pub async fn dismiss(db: impl PgExecutor<'_>, user_id: i64, id: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE users SET settings = jsonb_set(settings, '{dismissed_news}', to_jsonb($2::bigint))
         WHERE id = $1",
    )
    .bind(user_id)
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn current_news(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let old = create(&mut conn, None, "Old", None).await.unwrap();
        let expired = create(
            &mut conn,
            None,
            "Gone",
            Some(OffsetDateTime::now_utc() - time::Duration::hours(1)),
        )
        .await
        .unwrap();
        let new = create(&mut conn, None, "New", None).await.unwrap();
        let ids = |news: Vec<NewsUpdate>| news.into_iter().map(|n| n.id).collect::<Vec<_>>();
        assert_eq!(ids(current(&pool).await.unwrap()), [new, old]);
        assert!(set_deleted(&mut conn, new, true).await.unwrap());
        assert!(!set_deleted(&mut conn, new, true).await.unwrap());
        assert_eq!(ids(current(&pool).await.unwrap()), [old]);
        assert_eq!(
            ids(list(&pool, true, 0, 10).await.unwrap()),
            [new, expired, old]
        );
        assert!(update(&mut conn, old, "Older", None).await.unwrap());
        assert_eq!(by_id(&pool, old).await.unwrap().unwrap().body, "Older");
    }
}
