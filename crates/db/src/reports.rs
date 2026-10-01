//! Site statistics: daily counts of what happened (`daily_stats`), in all
//! and by user, and site-wide totals (`site_totals`), both filled by the
//! `stats.refresh` job.

use sqlx::{PgExecutor, PgPool};
use time::{Date, OffsetDateTime};

/// Something counted each day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Uploads,
    PostChanges,
    Approvals,
    Comments,
    ForumPosts,
    WikiEdits,
    NoteChanges,
    Favorites,
    Votes,
    Signups,
}

/// Where a metric's rows come from: the table, its user column (if it has
/// one) and any condition, all fixed strings of ours.
struct Source {
    table: &'static str,
    user: Option<&'static str>,
    filter: &'static str,
}

impl Metric {
    pub const ALL: [Metric; 10] = [
        Metric::Uploads,
        Metric::PostChanges,
        Metric::Approvals,
        Metric::Comments,
        Metric::ForumPosts,
        Metric::WikiEdits,
        Metric::NoteChanges,
        Metric::Favorites,
        Metric::Votes,
        Metric::Signups,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Metric::Uploads => "uploads",
            Metric::PostChanges => "post_changes",
            Metric::Approvals => "approvals",
            Metric::Comments => "comments",
            Metric::ForumPosts => "forum_posts",
            Metric::WikiEdits => "wiki_edits",
            Metric::NoteChanges => "note_changes",
            Metric::Favorites => "favorites",
            Metric::Votes => "votes",
            Metric::Signups => "signups",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Metric::Uploads => "Uploads",
            Metric::PostChanges => "Post changes",
            Metric::Approvals => "Approvals",
            Metric::Comments => "Comments",
            Metric::ForumPosts => "Forum posts",
            Metric::WikiEdits => "Wiki edits",
            Metric::NoteChanges => "Note changes",
            Metric::Favorites => "Favorites",
            Metric::Votes => "Post votes",
            Metric::Signups => "New accounts",
        }
    }

    /// Whether it's counted by user too.
    pub fn by_user(self) -> bool {
        self.source().user.is_some()
    }

    fn source(self) -> Source {
        let (table, user, filter) = match self {
            Metric::Uploads => ("posts", Some("uploader_id"), "true"),
            // The first version is the upload.
            Metric::PostChanges => ("post_versions", Some("updater_id"), "version > 1"),
            Metric::Approvals => ("mod_actions", Some("actor_id"), "action = 'post.approve'"),
            Metric::Comments => ("comments", Some("creator_id"), "true"),
            Metric::ForumPosts => ("forum_posts", Some("creator_id"), "true"),
            Metric::WikiEdits => ("wiki_page_versions", Some("updater_id"), "true"),
            Metric::NoteChanges => ("note_versions", Some("updater_id"), "true"),
            Metric::Favorites => ("favorites", Some("user_id"), "true"),
            Metric::Votes => ("post_votes", Some("user_id"), "true"),
            Metric::Signups => ("users", None, "true"),
        };
        Source {
            table,
            user,
            filter,
        }
    }
}

/// Recounts `metric` for the UTC days `from` to `to`, inclusive.
pub async fn refresh(db: &PgPool, metric: Metric, from: Date, to: Date) -> sqlx::Result<()> {
    let Source {
        table,
        user,
        filter,
    } = metric.source();
    // Totals get user 0; rows by users since deleted only count in them.
    let (user_col, user_id, groups) = match user {
        Some(col) => (
            col,
            "CASE WHEN grouping(uid) = 1 THEN 0 ELSE uid END",
            "GROUP BY GROUPING SETS ((day, uid), (day)) HAVING grouping(uid) = 1 OR uid IS NOT NULL",
        ),
        None => ("NULL::bigint", "0", "GROUP BY day"),
    };
    let mut tx = db.begin().await?;
    sqlx::query("DELETE FROM daily_stats WHERE metric = $1 AND day BETWEEN $2 AND $3")
        .bind(metric.as_str())
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO daily_stats (metric, day, user_id, count)
         SELECT $1, day, {user_id}, count(*)
         FROM (SELECT (created_at AT TIME ZONE 'UTC')::date AS day, {user_col} AS uid
               FROM {table}
               WHERE {filter}
                 AND created_at >= $2::date::timestamp AT TIME ZONE 'UTC'
                 AND created_at < ($3::date + 1)::timestamp AT TIME ZONE 'UTC') AS t
         {groups}"
    )))
    .bind(metric.as_str())
    .bind(from)
    .bind(to)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// The first day counted for `metric`, if any is.
pub async fn first_day(db: impl PgExecutor<'_>, metric: Metric) -> sqlx::Result<Option<Date>> {
    sqlx::query_scalar("SELECT min(day) FROM daily_stats WHERE metric = $1")
        .bind(metric.as_str())
        .fetch_one(db)
        .await
}

/// `metric`'s count on each day from `from` to `to`, oldest first, zeros
/// included: in all, or by `user_id`.
pub async fn daily(
    db: impl PgExecutor<'_>,
    metric: Metric,
    user_id: Option<i64>,
    from: Date,
    to: Date,
) -> sqlx::Result<Vec<(Date, i64)>> {
    sqlx::query_as(
        "SELECT d::date, coalesce(s.count, 0)
         FROM generate_series($3::date, $4::date, interval '1 day') AS d
         LEFT JOIN daily_stats s ON s.metric = $1 AND s.day = d::date AND s.user_id = $2
         ORDER BY d",
    )
    .bind(metric.as_str())
    .bind(user_id.unwrap_or(0))
    .bind(from)
    .bind(to)
    .fetch_all(db)
    .await
}

/// Those who did `metric` most from `from` to `to`, as `(name, count)`.
pub async fn top_users(
    db: impl PgExecutor<'_>,
    metric: Metric,
    from: Date,
    to: Date,
    limit: i64,
) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as(
        "SELECT u.name::text, sum(s.count)::bigint AS total
         FROM daily_stats s JOIN users u ON u.id = s.user_id
         WHERE s.metric = $1 AND s.day BETWEEN $2 AND $3 AND s.user_id <> 0
         GROUP BY u.id, u.name ORDER BY total DESC, u.name LIMIT $4",
    )
    .bind(metric.as_str())
    .bind(from)
    .bind(to)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The site-wide totals, in the order they're shown.
pub const TOTALS: [(&str, &str); 10] = [
    (
        "posts",
        "SELECT count(*) FROM posts WHERE status <> 'deleted'",
    ),
    ("tags", "SELECT count(*) FROM tags WHERE post_count > 0"),
    (
        "users",
        "SELECT count(*) FROM users WHERE status = 'active'",
    ),
    ("favorites", "SELECT count(*) FROM favorites"),
    (
        "comments",
        "SELECT count(*) FROM comments WHERE NOT is_deleted",
    ),
    (
        "forum_posts",
        "SELECT count(*) FROM forum_posts WHERE NOT is_hidden",
    ),
    ("wiki_pages", "SELECT count(*) FROM wiki_pages"),
    ("pools", "SELECT count(*) FROM pools WHERE NOT is_deleted"),
    (
        "artists",
        "SELECT count(*) FROM artists WHERE NOT is_deleted",
    ),
    ("notes", "SELECT count(*) FROM notes WHERE is_active"),
];

/// Counts the site-wide totals again.
pub async fn refresh_totals(db: &PgPool) -> sqlx::Result<()> {
    for (key, query) in TOTALS {
        let value: i64 = sqlx::query_scalar(query).fetch_one(db).await?;
        sqlx::query(
            "INSERT INTO site_totals (key, value) VALUES ($1, $2)
             ON CONFLICT (key) DO UPDATE SET value = $2, counted_at = now()",
        )
        .bind(key)
        .bind(value)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// The totals as last counted, with when, in [`TOTALS`] order; empty
/// before the first count.
pub async fn totals(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<(String, i64, OffsetDateTime)>> {
    let keys: Vec<&str> = TOTALS.iter().map(|(key, _)| *key).collect();
    sqlx::query_as(
        "SELECT key, value, counted_at FROM site_totals WHERE key = ANY($1)
         ORDER BY array_position($1, key)",
    )
    .bind(&keys)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn counts_days_and_users(pool: PgPool) {
        let alice: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        for (uploader, days_ago) in [
            (Some(alice), 0),
            (Some(alice), 0),
            (None, 0),
            (Some(alice), 2),
        ] {
            sqlx::query(
                "INSERT INTO posts (rating, uploader_id, created_at)
                 VALUES ('g', $1, now() - make_interval(days => $2))",
            )
            .bind(uploader)
            .bind(days_ago)
            .execute(&pool)
            .await
            .unwrap();
        }
        let today = OffsetDateTime::now_utc().date();
        let from = today - Duration::days(2);
        for metric in Metric::ALL {
            refresh(&pool, metric, from, today).await.unwrap();
        }
        let all = daily(&pool, Metric::Uploads, None, from, today)
            .await
            .unwrap();
        assert_eq!(all.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [1, 0, 3]);
        let hers = daily(&pool, Metric::Uploads, Some(alice), from, today)
            .await
            .unwrap();
        assert_eq!(hers.iter().map(|(_, n)| *n).collect::<Vec<_>>(), [1, 0, 2]);
        assert_eq!(
            top_users(&pool, Metric::Uploads, from, today, 5)
                .await
                .unwrap(),
            [("alice".to_owned(), 3)]
        );
        assert_eq!(first_day(&pool, Metric::Uploads).await.unwrap(), Some(from));
        let signups = daily(&pool, Metric::Signups, None, today, today)
            .await
            .unwrap();
        assert_eq!(signups[0].1, 1);
        // Recounting replaces.
        refresh(&pool, Metric::Uploads, today, today).await.unwrap();
        assert_eq!(
            daily(&pool, Metric::Uploads, None, today, today)
                .await
                .unwrap()[0]
                .1,
            3
        );

        assert!(totals(&pool).await.unwrap().is_empty());
        refresh_totals(&pool).await.unwrap();
        let counted = totals(&pool).await.unwrap();
        assert_eq!(counted.len(), TOTALS.len());
        assert_eq!((counted[0].0.as_str(), counted[0].1), ("posts", 4));
    }
}
