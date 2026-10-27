//! Reports to staff about messages, forum posts, users, wiki pages, pools,
//! notes, artists and feedback (`moderation_reports`). Each is its own
//! row, keeping the reported text as it was then.

use moekura_core::moderation::{REPORT_COOLDOWN_DAYS, ReportKind};
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;

/// A report to make.
#[derive(Debug, Clone)]
pub struct NewReport<'a> {
    pub kind: ReportKind,
    pub subject_id: i64,
    /// Who wrote or made the thing, if anyone.
    pub owner_id: Option<i64>,
    pub creator_id: i64,
    pub reason: &'a str,
    /// The thing's text as it is now.
    pub snapshot: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("You already reported this; staff will look at it.")]
    AlreadyReported,
    #[error(
        "Staff settled your report about this lately; wait a few days before reporting it again."
    )]
    TooSoon,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Whether `creator_id` may report the thing now: they have no open
/// report about it, and none settled in the past
/// [`REPORT_COOLDOWN_DAYS`].
pub async fn check_new(
    db: impl PgExecutor<'_>,
    kind: ReportKind,
    subject_id: i64,
    creator_id: i64,
) -> Result<(), ReportError> {
    let (open, recent): (bool, bool) = sqlx::query_as(
        "SELECT coalesce(bool_or(status = 'open'), false),
                coalesce(bool_or(status <> 'open'
                                 AND resolved_at > now() - make_interval(days => $4)), false)
         FROM moderation_reports
         WHERE subject_kind = $1 AND subject_id = $2 AND creator_id = $3",
    )
    .bind(kind.as_str())
    .bind(subject_id)
    .bind(creator_id)
    .bind(REPORT_COOLDOWN_DAYS as i32)
    .fetch_one(db)
    .await?;
    if open {
        Err(ReportError::AlreadyReported)
    } else if recent {
        Err(ReportError::TooSoon)
    } else {
        Ok(())
    }
}

/// Makes a report, checking [`check_new`] again; returns its id.
pub async fn create(conn: &mut PgConnection, report: NewReport<'_>) -> Result<i64, ReportError> {
    check_new(
        &mut *conn,
        report.kind,
        report.subject_id,
        report.creator_id,
    )
    .await?;
    sqlx::query_scalar(
        "INSERT INTO moderation_reports (subject_kind, subject_id, owner_id, creator_id, reason,
                                         snapshot)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(report.kind.as_str())
    .bind(report.subject_id)
    .bind(report.owner_id)
    .bind(report.creator_id)
    .bind(report.reason)
    .bind(report.snapshot)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => ReportError::AlreadyReported,
        _ => ReportError::Db(e),
    })
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Report {
    pub id: i64,
    pub subject_kind: String,
    pub subject_id: i64,
    pub owner_id: Option<i64>,
    pub owner_name: Option<String>,
    pub creator_name: Option<String>,
    pub reason: String,
    pub snapshot: String,
    pub status: String,
    pub resolver_name: Option<String>,
    pub outcome: String,
    pub created_at: OffsetDateTime,
    pub resolved_at: Option<OffsetDateTime>,
}

/// `SELECT <report columns> …` followed by `$rest`.
macro_rules! select_reports {
    ($rest:literal) => {
        concat!(
            "SELECT r.id, r.subject_kind, r.subject_id, r.owner_id, o.name::text AS owner_name,
                    c.name::text AS creator_name, r.reason, r.snapshot, r.status,
                    x.name::text AS resolver_name, r.outcome, r.created_at, r.resolved_at
             FROM moderation_reports r
             LEFT JOIN users o ON o.id = r.owner_id
             LEFT JOIN users c ON c.id = r.creator_id
             LEFT JOIN users x ON x.id = r.resolver_id ",
            $rest
        )
    };
}

/// Open reports about up to `limit` things of `kinds`, the thing reported
/// longest ago first, each thing's reports together and oldest first.
/// Pages go by the id of a thing's oldest open report: pass the last
/// thing's as `after` for the next page (0 for the first).
pub async fn open(
    db: impl PgExecutor<'_>,
    kinds: &[ReportKind],
    after: i64,
    limit: i64,
) -> sqlx::Result<Vec<Report>> {
    let kinds: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    sqlx::query_as(select_reports!(
        "JOIN (SELECT subject_kind, subject_id, min(id) AS first FROM moderation_reports
               WHERE status = 'open' AND subject_kind = ANY($1)
               GROUP BY subject_kind, subject_id HAVING min(id) > $2
               ORDER BY first LIMIT $3) q
           ON q.subject_kind = r.subject_kind AND q.subject_id = r.subject_id
         WHERE r.status = 'open'
         ORDER BY q.first, r.id"
    ))
    .bind(kinds)
    .bind(after)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// How many things of `kinds` have open reports.
pub async fn count_open(db: impl PgExecutor<'_>, kinds: &[ReportKind]) -> sqlx::Result<i64> {
    let kinds: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
    sqlx::query_scalar(
        "SELECT count(DISTINCT (subject_kind, subject_id)) FROM moderation_reports
         WHERE status = 'open' AND subject_kind = ANY($1)",
    )
    .bind(kinds)
    .fetch_one(db)
    .await
}

/// Every report about one thing, oldest first.
pub async fn for_subject(
    db: impl PgExecutor<'_>,
    kind: ReportKind,
    subject_id: i64,
) -> sqlx::Result<Vec<Report>> {
    sqlx::query_as(select_reports!(
        "WHERE r.subject_kind = $1 AND r.subject_id = $2 ORDER BY r.id"
    ))
    .bind(kind.as_str())
    .bind(subject_id)
    .fetch_all(db)
    .await
}

/// Reports about what `owner_id` wrote or made, newest first.
pub async fn about_owner(
    db: impl PgExecutor<'_>,
    owner_id: i64,
    limit: i64,
) -> sqlx::Result<Vec<Report>> {
    sqlx::query_as(select_reports!(
        "WHERE r.owner_id = $1 ORDER BY r.id DESC LIMIT $2"
    ))
    .bind(owner_id)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Closes the open reports about one thing as `upheld` or `dismissed`,
/// with `outcome`; returns how many there were.
pub async fn resolve(
    db: impl PgExecutor<'_>,
    kind: ReportKind,
    subject_id: i64,
    upheld: bool,
    resolver_id: Option<i64>,
    outcome: &str,
) -> sqlx::Result<u64> {
    let done = sqlx::query(
        "UPDATE moderation_reports
         SET status = $3, resolver_id = $4, outcome = $5, resolved_at = now()
         WHERE subject_kind = $1 AND subject_id = $2 AND status = 'open'",
    )
    .bind(kind.as_str())
    .bind(subject_id)
    .bind(if upheld { "upheld" } else { "dismissed" })
    .bind(resolver_id)
    .bind(outcome)
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
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

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn reports_are_kept_one_by_one(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let carol = user(&pool, "carol").await;
        let report = |creator, reason| NewReport {
            kind: ReportKind::ForumPost,
            subject_id: 7,
            owner_id: Some(bob),
            creator_id: creator,
            reason,
            snapshot: "rude words",
        };
        let mut tx = pool.begin().await.unwrap();
        create(&mut tx, report(alice, "rude")).await.unwrap();
        assert!(matches!(
            create(&mut tx, report(alice, "again")).await,
            Err(ReportError::AlreadyReported)
        ));
        create(&mut tx, report(carol, "spam")).await.unwrap();
        tx.commit().await.unwrap();

        let queue = open(&pool, &[ReportKind::ForumPost], 0, 10).await.unwrap();
        assert_eq!(
            queue.iter().map(|r| r.reason.as_str()).collect::<Vec<_>>(),
            ["rude", "spam"]
        );
        assert_eq!(queue[0].snapshot, "rude words");
        assert!(
            open(&pool, &[ReportKind::Dmail], 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(count_open(&pool, &ReportKind::ALL).await.unwrap(), 1);

        assert_eq!(
            resolve(&pool, ReportKind::ForumPost, 7, false, Some(bob), "fine")
                .await
                .unwrap(),
            2
        );
        assert_eq!(count_open(&pool, &ReportKind::ALL).await.unwrap(), 0);
        // Settled lately: not again for a while.
        let mut tx = pool.begin().await.unwrap();
        assert!(matches!(
            create(&mut tx, report(alice, "still rude")).await,
            Err(ReportError::TooSoon)
        ));
        tx.rollback().await.unwrap();
        sqlx::query("UPDATE moderation_reports SET resolved_at = now() - interval '30 days'")
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        create(&mut tx, report(alice, "still rude")).await.unwrap();
        tx.commit().await.unwrap();
        let all = for_subject(&pool, ReportKind::ForumPost, 7).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].outcome, "fine");
        assert_eq!(about_owner(&pool, bob, 10).await.unwrap().len(), 3);
    }
}
