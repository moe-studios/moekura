//! Banned users' appeals of their ban (`ban_appeals`).

use moekura_core::moderation::REPORT_COOLDOWN_DAYS;
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

#[derive(Debug, thiserror::Error)]
pub enum AppealError {
    #[error("You're not banned.")]
    NotBanned,
    #[error("You already appealed this ban; staff will look at it.")]
    AlreadyAppealed,
    #[error(
        "Staff turned down an appeal of this ban lately; wait a few days before appealing again."
    )]
    TooSoon,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BanAppeal {
    pub id: i64,
    pub ban_id: i64,
    pub user_id: i64,
    pub user_name: String,
    pub reason: String,
    pub status: String,
    pub resolver_name: Option<String>,
    pub outcome: String,
    pub created_at: OffsetDateTime,
    /// The ban's own reason, length and who set it.
    pub ban_reason: String,
    pub ban_expires_at: Option<OffsetDateTime>,
    pub banner_id: Option<i64>,
    pub banner_name: Option<String>,
}

/// `SELECT <appeal columns> …` followed by `$rest`.
macro_rules! select_appeals {
    ($rest:literal) => {
        concat!(
            "SELECT a.id, a.ban_id, a.user_id, u.name::text AS user_name, a.reason, a.status,
                    r.name::text AS resolver_name, a.outcome, a.created_at,
                    b.reason AS ban_reason, b.expires_at AS ban_expires_at, b.banner_id,
                    x.name::text AS banner_name
             FROM ban_appeals a
             JOIN users u ON u.id = a.user_id
             JOIN bans b ON b.id = a.ban_id
             LEFT JOIN users r ON r.id = a.resolver_id
             LEFT JOIN users x ON x.id = b.banner_id ",
            $rest
        )
    };
}

/// Whether `user_id` may appeal their ban now, returning the ban's id:
/// they're banned, with no open appeal of it, and none turned down
/// lately.
pub async fn check_new(conn: &mut PgConnection, user_id: i64) -> Result<i64, AppealError> {
    let ban: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM bans
         WHERE user_id = $1 AND lifted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
         ORDER BY id DESC LIMIT 1 FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?;
    let ban = ban.ok_or(AppealError::NotBanned)?;
    let (open, recent): (bool, bool) = sqlx::query_as(
        "SELECT coalesce(bool_or(status = 'open'), false),
                coalesce(bool_or(status = 'denied'
                                 AND resolved_at > now() - make_interval(days => $2)), false)
         FROM ban_appeals WHERE ban_id = $1",
    )
    .bind(ban)
    .bind(REPORT_COOLDOWN_DAYS as i32)
    .fetch_one(&mut *conn)
    .await?;
    if open {
        return Err(AppealError::AlreadyAppealed);
    }
    if recent {
        return Err(AppealError::TooSoon);
    }
    Ok(ban)
}

/// Appeals `user_id`'s ban in force, with `reason`, checking
/// [`check_new`]; returns the appeal's id.
pub async fn create(
    conn: &mut PgConnection,
    user_id: i64,
    reason: &str,
) -> Result<i64, AppealError> {
    let ban = check_new(&mut *conn, user_id).await?;
    Ok(sqlx::query_scalar(
        "INSERT INTO ban_appeals (ban_id, user_id, reason) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(ban)
    .bind(user_id)
    .bind(reason)
    .fetch_one(&mut *conn)
    .await?)
}

/// Open appeals, oldest first.
pub async fn open(db: impl PgExecutor<'_>, limit: i64) -> sqlx::Result<Vec<BanAppeal>> {
    sqlx::query_as(select_appeals!(
        "WHERE a.status = 'open' ORDER BY a.id LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(db)
    .await
}

/// `user_id`'s appeals, newest first.
pub async fn for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<BanAppeal>> {
    sqlx::query_as(select_appeals!("WHERE a.user_id = $1 ORDER BY a.id DESC"))
        .bind(user_id)
        .fetch_all(db)
        .await
}

/// Appeal `id`, locked until the transaction ends.
pub async fn lock(conn: &mut PgConnection, id: i64) -> sqlx::Result<Option<BanAppeal>> {
    sqlx::query_as(select_appeals!("WHERE a.id = $1 FOR UPDATE OF a"))
        .bind(id)
        .fetch_optional(conn)
        .await
}

/// Decides open appeal `id`; false if it was decided already.
pub async fn resolve(
    db: impl PgExecutor<'_>,
    id: i64,
    granted: bool,
    resolver_id: Option<i64>,
    outcome: &str,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE ban_appeals SET status = $2, resolver_id = $3, outcome = $4, resolved_at = now()
         WHERE id = $1 AND status = 'open'",
    )
    .bind(id)
    .bind(if granted { "granted" } else { "denied" })
    .bind(resolver_id)
    .bind(outcome)
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn one_open_appeal_and_a_wait_after_a_no(pool: PgPool) {
        let alice: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        assert!(matches!(
            create(&mut conn, alice, "why?").await,
            Err(AppealError::NotBanned)
        ));
        crate::bans::ban(&mut conn, alice, "spam", None, None, None)
            .await
            .unwrap();
        let id = create(&mut conn, alice, "it wasn't me").await.unwrap();
        assert!(matches!(
            create(&mut conn, alice, "really").await,
            Err(AppealError::AlreadyAppealed)
        ));
        assert_eq!(open(&pool, 10).await.unwrap()[0].ban_reason, "spam");
        assert!(resolve(&pool, id, false, None, "it was").await.unwrap());
        assert!(!resolve(&pool, id, true, None, "").await.unwrap());
        assert!(matches!(
            create(&mut conn, alice, "please").await,
            Err(AppealError::TooSoon)
        ));
        assert_eq!(for_user(&pool, alice).await.unwrap()[0].outcome, "it was");
    }
}
