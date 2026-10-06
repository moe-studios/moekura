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
    /// Tags separated by spaces, an excluded one after `-` and an optional
    /// one after `~`; a pattern has a `*`.
    pub query: String,
    pub searches: i32,
    pub misses: i32,
}

/// Adds `searches` to each search's counts for its day.
///
/// A search naming a tag that doesn't exist (neither a tag nor an alias of
/// one), or with a pattern among its excluded or optional tags, counts
/// only the times it found nothing: what it found came from its other
/// tags, and the searches that found posts most (see [`top_searches`])
/// shouldn't show made-up tags. A pattern it requires found posts only by
/// matching tags.
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
         SELECT day, query, CASE WHEN known THEN searches ELSE misses END, misses
         FROM (
             SELECT s.*, NOT EXISTS (
                 SELECT 1 FROM unnest(string_to_array(s.query, ' ')) AS t (term)
                 WHERE CASE
                     WHEN strpos(t.term, '*') > 0 THEN left(t.term, 1) IN ('-', '~')
                     ELSE NOT EXISTS (SELECT 1 FROM tags WHERE name = ltrim(t.term, '-~'))
                         AND NOT EXISTS (
                             SELECT 1 FROM tag_relations
                             WHERE kind = 'alias' AND status = 'active'
                                 AND antecedent_name = ltrim(t.term, '-~')
                         )
                 END
             ) AS known
             FROM unnest($1::date[], $2::text[], $3::int[], $4::int[])
                 AS s (day, query, searches, misses)
         ) s
         WHERE known OR misses > 0
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

/// The searches that most often found posts from `from` to `to`, or with
/// `missed`, those that most often found nothing, with their counts.
pub async fn top_searches(
    db: impl PgExecutor<'_>,
    from: Date,
    to: Date,
    missed: bool,
    limit: i64,
) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as(
        "SELECT query, n FROM (
             SELECT query,
                 sum(CASE WHEN $3 THEN misses ELSE searches - misses END)::bigint AS n
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
        sqlx::raw_sql(
            "INSERT INTO tags (name) VALUES ('cat'), ('dog');
             INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('alias', 'kitty', 'cat', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();
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
            [("cat".to_owned(), 3)]
        );
        assert_eq!(
            top_searches(&pool, day, next, true, 10).await.unwrap(),
            [("dgo".to_owned(), 3)]
        );

        // Searches naming tags that don't exist only count their misses.
        add_searches(
            &pool,
            &[
                searched("kitty -dog", 2, 0),
                searched("long_* -dog", 2, 0),
                searched("cat dog", 2, 2),
                searched("cat -made_up", 4, 0),
                searched("cat -spam_*", 5, 0),
                searched("~cat ~made_up", 2, 1),
            ],
        )
        .await
        .unwrap();
        let pairs = |list: &[(&str, i64)]| -> Vec<(String, i64)> {
            list.iter().map(|(q, n)| ((*q).to_owned(), *n)).collect()
        };
        assert_eq!(
            top_searches(&pool, day, next, false, 10).await.unwrap(),
            pairs(&[("cat", 3), ("kitty -dog", 2), ("long_* -dog", 2)])
        );
        assert_eq!(
            top_searches(&pool, day, next, true, 10).await.unwrap(),
            pairs(&[("dgo", 3), ("cat dog", 2), ("~cat ~made_up", 1)])
        );
        // What found posts but named a made-up tag isn't kept at all.
        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM search_counts")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kept, 6);

        assert_eq!(prune(&pool, next).await.unwrap(), 8);
        assert_eq!(most_viewed(&pool, day, next, 10).await.unwrap(), [(b, 3)]);
    }
}
