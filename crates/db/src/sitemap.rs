//! What the sitemap lists. Each sitemap file covers a fixed range of ids,
//! so a file is one index range scan however large the site, and the
//! sitemap index only needs each table's highest id.

use sqlx::PgExecutor;
use time::OffsetDateTime;

/// Ids each sitemap file covers (a file may list at most 50,000 URLs).
pub const PER_FILE: i64 = 10_000;

/// The kinds of page the sitemap lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Posts,
    Tags,
    Wiki,
    Pools,
    Artists,
    Topics,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Posts,
        Kind::Tags,
        Kind::Wiki,
        Kind::Pools,
        Kind::Artists,
        Kind::Topics,
    ];

    /// As in sitemap file names.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Posts => "posts",
            Kind::Tags => "tags",
            Kind::Wiki => "wiki",
            Kind::Pools => "pools",
            Kind::Artists => "artists",
            Kind::Topics => "forum",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    fn table(self) -> &'static str {
        match self {
            Kind::Posts => "posts",
            Kind::Tags => "tags",
            Kind::Wiki => "wiki_pages",
            Kind::Pools => "pools",
            Kind::Artists => "artists",
            Kind::Topics => "forum_topics",
        }
    }
}

/// How many files `kind` needs.
pub async fn files(db: impl PgExecutor<'_>, kind: Kind) -> sqlx::Result<i64> {
    // The table name is one of ours, never input.
    let max: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT coalesce(max(id), 0)::bigint FROM {}",
        kind.table()
    )))
    .fetch_one(db)
    .await?;
    Ok((max + PER_FILE - 1) / PER_FILE)
}

/// One page in a sitemap: what identifies it (an id or a name) and when
/// it last changed, if known.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Entry {
    pub key: String,
    pub updated_at: Option<OffsetDateTime>,
}

/// What visitors may see of posts.
#[derive(Debug, Clone, Default)]
pub struct PostFilter {
    /// Ratings visitors see; empty for all.
    pub ratings: Vec<String>,
    /// Posts with any of these tags are hidden (banned artists').
    pub hidden_tags: Vec<i32>,
}

/// The entries of `kind`'s file number `file` (from 0).
pub async fn entries(
    db: impl PgExecutor<'_>,
    kind: Kind,
    file: i64,
    posts: &PostFilter,
) -> sqlx::Result<Vec<Entry>> {
    let (from, to) = (file * PER_FILE + 1, (file + 1) * PER_FILE);
    let query = match kind {
        Kind::Posts => {
            return sqlx::query_as(
                "SELECT id::text AS key, updated_at FROM posts
                 WHERE id BETWEEN $1 AND $2 AND status IN ('active', 'flagged')
                   AND (cardinality($3::text[]) = 0 OR rating = ANY($3))
                   AND NOT tag_ids && $4::int[]
                 ORDER BY id",
            )
            .bind(from)
            .bind(to)
            .bind(&posts.ratings)
            .bind(&posts.hidden_tags)
            .fetch_all(db)
            .await;
        }
        Kind::Tags => {
            "SELECT name AS key, NULL::timestamptz AS updated_at FROM tags
             WHERE id BETWEEN $1 AND $2 AND post_count > 0 AND NOT is_deprecated ORDER BY id"
        }
        Kind::Wiki => {
            "SELECT title AS key, updated_at FROM wiki_pages
             WHERE id BETWEEN $1 AND $2 ORDER BY id"
        }
        Kind::Pools => {
            "SELECT id::text AS key, updated_at FROM pools
             WHERE id BETWEEN $1 AND $2 AND NOT is_deleted ORDER BY id"
        }
        Kind::Artists => {
            "SELECT id::text AS key, updated_at FROM artists
             WHERE id BETWEEN $1 AND $2 AND NOT is_deleted AND NOT is_banned ORDER BY id"
        }
        Kind::Topics => {
            "SELECT id::text AS key, updated_at FROM forum_topics
             WHERE id BETWEEN $1 AND $2 AND NOT is_deleted AND NOT is_held
               AND merged_into_id IS NULL
             ORDER BY id"
        }
    };
    sqlx::query_as(query)
        .bind(from)
        .bind(to)
        .fetch_all(db)
        .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn lists_what_visitors_see(pool: PgPool) {
        let banned: i32 =
            sqlx::query_scalar("INSERT INTO tags (name) VALUES ('banned') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let mut ids = Vec::new();
        for (rating, status, tags) in [
            ("g", "active", vec![]),
            ("e", "active", vec![]),
            ("g", "deleted", vec![]),
            ("g", "active", vec![banned]),
        ] {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO posts (rating, status, tag_ids) VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(rating)
            .bind(status)
            .bind(tags)
            .fetch_one(&pool)
            .await
            .unwrap();
            ids.push(id.to_string());
        }
        assert_eq!(files(&pool, Kind::Posts).await.unwrap(), 1);
        assert_eq!(files(&pool, Kind::Pools).await.unwrap(), 0);
        let filter = PostFilter {
            ratings: vec!["g".into()],
            hidden_tags: vec![banned],
        };
        let found = entries(&pool, Kind::Posts, 0, &filter).await.unwrap();
        assert_eq!(
            found.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
            [ids[0].as_str()]
        );
        assert!(
            entries(&pool, Kind::Posts, 1, &filter)
                .await
                .unwrap()
                .is_empty()
        );
        // Tags only while they're used.
        sqlx::query("INSERT INTO tags (name) VALUES ('unused')")
            .execute(&pool)
            .await
            .unwrap();
        let tags = entries(&pool, Kind::Tags, 0, &filter).await.unwrap();
        assert_eq!(
            tags.iter().map(|e| e.key.as_str()).collect::<Vec<_>>(),
            ["banned"]
        );
        for kind in Kind::ALL {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
            entries(&pool, kind, 0, &filter).await.unwrap();
        }
    }
}
