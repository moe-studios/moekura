//! Counts of post views and searches per day (`post_views`,
//! `search_counts`), for the explore pages.

use sqlx::{PgExecutor, PgPool};
use time::Date;

/// Adds `views` to each post's count for its day. Posts deleted for
/// good since are skipped.
pub async fn add_views(db: &PgPool, views: &[(Date, i64, i32)]) -> sqlx::Result<()> {
    if views.is_empty() {
        return Ok(());
    }
    let days: Vec<Date> = views.iter().map(|v| v.0).collect();
    let posts: Vec<i64> = views.iter().map(|v| v.1).collect();
    let counts: Vec<i32> = views.iter().map(|v| v.2).collect();
    sqlx::query(
        "INSERT INTO post_views (day, post_id, views)
         SELECT v.day, v.post_id, v.views
         FROM unnest($1::date[], $2::bigint[], $3::int[]) AS v (day, post_id, views)
         JOIN posts p ON p.id = v.post_id
         ORDER BY v.day, v.post_id
         ON CONFLICT (day, post_id) DO UPDATE SET views = post_views.views + EXCLUDED.views",
    )
    .bind(days)
    .bind(posts)
    .bind(counts)
    .execute(db)
    .await?;
    Ok(())
}

/// A search and how often it was made, and found nothing, on a day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Searched {
    pub day: Date,
    pub query: String,
    pub searches: i32,
    pub misses: i32,
}

pub async fn add_searches(db: &PgPool, searches: &[Searched]) -> sqlx::Result<()> {
    if searches.is_empty() {
        return Ok(());
    }
    let days: Vec<Date> = searches.iter().map(|s| s.day).collect();
    let queries: Vec<&str> = searches.iter().map(|s| s.query.as_str()).collect();
    let counts: Vec<i32> = searches.iter().map(|s| s.searches).collect();
    let misses: Vec<i32> = searches.iter().map(|s| s.misses).collect();
    sqlx::query(
        "INSERT INTO search_counts (day, query, searches, misses)
         SELECT * FROM unnest($1::date[], $2::text[], $3::int[], $4::int[])
         ORDER BY 1, 2
         ON CONFLICT (day, query) DO UPDATE
         SET searches = search_counts.searches + EXCLUDED.searches,
             misses = search_counts.misses + EXCLUDED.misses",
    )
    .bind(days)
    .bind(queries)
    .bind(counts)
    .bind(misses)
    .execute(db)
    .await?;
    Ok(())
}

/// The most viewed posts from `from` to `to` (inclusive), with their
/// views, most first.
pub async fn most_viewed(
    db: impl PgExecutor<'_>,
    from: Date,
    to: Date,
    limit: i64,
) -> sqlx::Result<Vec<(i64, i64)>> {
    sqlx::query_as(
        "SELECT post_id, sum(views)::bigint AS views FROM post_views
         WHERE day BETWEEN $1 AND $2
         GROUP BY post_id ORDER BY views DESC, post_id DESC LIMIT $3",
    )
    .bind(from)
    .bind(to)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// The searches made most from `from` to `to`, or with `missed`, those
/// that most often found nothing, with their counts.
pub async fn top_searches(
    db: impl PgExecutor<'_>,
    from: Date,
    to: Date,
    missed: bool,
    limit: i64,
) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as(
        "SELECT query, n FROM (
             SELECT query, sum(CASE WHEN $3 THEN misses ELSE searches END)::bigint AS n
             FROM search_counts WHERE day BETWEEN $1 AND $2 GROUP BY query
         ) q WHERE n > 0 ORDER BY n DESC, query LIMIT $4",
    )
    .bind(from)
    .bind(to)
    .bind(missed)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// Forgets counts from before `before`.
pub async fn prune(db: &PgPool, before: Date) -> sqlx::Result<u64> {
    let views = sqlx::query("DELETE FROM post_views WHERE day < $1")
        .bind(before)
        .execute(db)
        .await?
        .rows_affected();
    let searches = sqlx::query("DELETE FROM search_counts WHERE day < $1")
        .bind(before)
        .execute(db)
        .await?
        .rows_affected();
    Ok(views + searches)
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::*;

    async fn post(pool: &PgPool) -> i64 {
        sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn counts_add_up(pool: PgPool) {
        let a = post(&pool).await;
        let b = post(&pool).await;
        let day = date!(2026 - 09 - 01);
        let next = date!(2026 - 09 - 02);
        add_views(
            &pool,
            &[(day, a, 2), (day, b, 1), (next, b, 3), (day, 999_999, 5)],
        )
        .await
        .unwrap();
        add_views(&pool, &[(day, a, 1)]).await.unwrap();
        assert_eq!(
            most_viewed(&pool, day, day, 10).await.unwrap(),
            [(a, 3), (b, 1)]
        );
        assert_eq!(
            most_viewed(&pool, day, next, 10).await.unwrap(),
            [(b, 4), (a, 3)]
        );

        let searched = |query: &str, searches, misses| Searched {
            day,
            query: query.into(),
            searches,
            misses,
        };
        add_searches(&pool, &[searched("cat", 3, 0), searched("dgo", 2, 2)])
            .await
            .unwrap();
        add_searches(&pool, &[searched("dgo", 1, 1)]).await.unwrap();
        assert_eq!(
            top_searches(&pool, day, next, false, 10).await.unwrap(),
            [("cat".to_owned(), 3), ("dgo".to_owned(), 3)]
        );
        assert_eq!(
            top_searches(&pool, day, next, true, 10).await.unwrap(),
            [("dgo".to_owned(), 3)]
        );

        assert_eq!(prune(&pool, next).await.unwrap(), 4);
        assert_eq!(most_viewed(&pool, day, next, 10).await.unwrap(), [(b, 3)]);
    }
}
