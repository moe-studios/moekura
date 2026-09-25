//! Queries on `tags` and `tag_categories`.

use sqlx::{PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Category {
    pub id: i16,
    pub name: String,
    pub label: String,
    pub position: i16,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Tag {
    pub id: i32,
    pub name: String,
    pub category_id: i16,
    pub post_count: i32,
    pub is_deprecated: bool,
    pub created_at: OffsetDateTime,
}

/// `SELECT <tag columns> FROM tags` followed by `$rest`.
macro_rules! select_tags {
    ($rest:literal) => {
        concat!(
            "SELECT id, name, category_id, post_count, is_deprecated, created_at FROM tags ",
            $rest
        )
    };
}

/// All categories, in display order.
pub async fn categories(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<Category>> {
    sqlx::query_as("SELECT id, name, label, position FROM tag_categories ORDER BY position, id")
        .fetch_all(db)
        .await
}

pub async fn by_name(db: impl PgExecutor<'_>, name: &str) -> sqlx::Result<Option<Tag>> {
    sqlx::query_as(select_tags!("WHERE name = $1"))
        .bind(name)
        .fetch_optional(db)
        .await
}

/// The tags among `names` that exist, in no particular order.
pub async fn by_names(db: impl PgExecutor<'_>, names: &[&str]) -> sqlx::Result<Vec<Tag>> {
    sqlx::query_as(select_tags!("WHERE name = ANY($1)"))
        .bind(names)
        .fetch_all(db)
        .await
}

/// The tags with the given ids, most used first.
pub async fn by_ids(db: impl PgExecutor<'_>, ids: &[i32]) -> sqlx::Result<Vec<Tag>> {
    sqlx::query_as(select_tags!(
        "WHERE id = ANY($1) ORDER BY post_count DESC, name"
    ))
    .bind(ids)
    .fetch_all(db)
    .await
}

/// A tag wanted on a post, with the category to give it if it is new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedTag<'a> {
    pub name: &'a str,
    pub category_id: Option<i16>,
}

/// Finds the tags named in `wanted`, creating missing ones. A requested
/// category is applied to new tags and to unused ones (no posts yet), or
/// to any tag when `recategorize` is set.
pub async fn ensure(
    conn: &mut PgConnection,
    wanted: &[WantedTag<'_>],
    recategorize: bool,
) -> sqlx::Result<Vec<Tag>> {
    let names: Vec<&str> = wanted.iter().map(|w| w.name).collect();
    let categories: Vec<Option<i16>> = wanted.iter().map(|w| w.category_id).collect();
    // Sorted, so concurrent uploads creating the same tags take the same
    // locks in the same order.
    sqlx::query(
        "INSERT INTO tags (name, category_id)
         SELECT name, coalesce(category_id, 0)
         FROM unnest($1::text[], $2::int2[]) AS wanted (name, category_id)
         ORDER BY name
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(&names)
    .bind(&categories)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE tags SET category_id = wanted.category_id
         FROM unnest($1::text[], $2::int2[]) AS wanted (name, category_id)
         WHERE tags.name = wanted.name
           AND tags.category_id <> wanted.category_id
           AND (tags.post_count = 0 OR $3)",
    )
    .bind(&names)
    .bind(&categories)
    .bind(recategorize)
    .execute(&mut *conn)
    .await?;
    by_names(&mut *conn, &names).await
}

/// The tags a post gets for `wanted`: aliases resolved, missing tags
/// created (see [`ensure`]), and implied tags added. Sorted by id.
pub async fn for_post(
    conn: &mut PgConnection,
    wanted: &[WantedTag<'_>],
    recategorize: bool,
) -> sqlx::Result<Vec<Tag>> {
    let names: Vec<&str> = wanted.iter().map(|w| w.name).collect();
    let aliases = crate::tag_relations::aliases_of(&mut *conn, &names).await?;
    let mut resolved: Vec<WantedTag<'_>> = Vec::with_capacity(wanted.len());
    for w in wanted {
        let name = aliases
            .iter()
            .find(|(antecedent, _)| antecedent == w.name)
            .map_or(w.name, |(_, consequent)| consequent.as_str());
        if !resolved.iter().any(|r| r.name == name) {
            resolved.push(WantedTag {
                name,
                category_id: w.category_id,
            });
        }
    }
    let mut tags = ensure(&mut *conn, &resolved, recategorize).await?;
    let present: Vec<&str> = tags.iter().map(|t| t.name.as_str()).collect();
    let implied = crate::tag_relations::implied_by(&mut *conn, &present).await?;
    if !implied.is_empty() {
        let extra: Vec<WantedTag<'_>> = implied
            .iter()
            .map(|name| WantedTag {
                name,
                category_id: None,
            })
            .collect();
        tags.extend(ensure(&mut *conn, &extra, false).await?);
    }
    tags.sort_by_key(|t| t.id);
    tags.dedup_by_key(|t| t.id);
    Ok(tags)
}

/// Changes a tag's category and deprecation. Returns false if there is no
/// such tag.
pub async fn update(
    db: impl PgExecutor<'_>,
    id: i32,
    category_id: i16,
    is_deprecated: bool,
) -> sqlx::Result<bool> {
    let result = sqlx::query("UPDATE tags SET category_id = $2, is_deprecated = $3 WHERE id = $1")
        .bind(id)
        .bind(category_id)
        .bind(is_deprecated)
        .execute(db)
        .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<Option<Tag>> {
    sqlx::query_as(select_tags!("WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// How [`list`] orders tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListOrder {
    #[default]
    Count,
    Name,
    Newest,
}

/// A page of tags whose names match `pattern` (a prefix, or a pattern
/// with `*` wildcards), optionally only in one category.
pub async fn list(
    db: impl PgExecutor<'_>,
    pattern: &str,
    category_id: Option<i16>,
    order: ListOrder,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Tag>> {
    let like = like_pattern(pattern);
    let query = match order {
        ListOrder::Count => select_tags!(
            "WHERE name LIKE $1 AND ($2::int2 IS NULL OR category_id = $2)
             ORDER BY post_count DESC, id OFFSET $3 LIMIT $4"
        ),
        ListOrder::Name => select_tags!(
            "WHERE name LIKE $1 AND ($2::int2 IS NULL OR category_id = $2)
             ORDER BY name OFFSET $3 LIMIT $4"
        ),
        ListOrder::Newest => select_tags!(
            "WHERE name LIKE $1 AND ($2::int2 IS NULL OR category_id = $2)
             ORDER BY id DESC OFFSET $3 LIMIT $4"
        ),
    };
    sqlx::query_as(query)
        .bind(like)
        .bind(category_id)
        .bind(offset)
        .bind(limit)
        .fetch_all(db)
        .await
}

/// Which tags [`filter`] finds; every condition given must hold.
#[derive(Debug, Clone, Default)]
pub struct TagFilter<'a> {
    /// Exactly these names.
    pub names: Option<&'a [String]>,
    /// A name pattern where `*` matches anything; without one, the whole
    /// name.
    pub pattern: Option<&'a str>,
    /// In one of these categories (any if empty).
    pub categories: &'a [i16],
    /// Only tags on at least one post.
    pub used_only: bool,
}

/// A page of tags matching `filter`.
pub async fn filter(
    db: impl PgExecutor<'_>,
    filter: &TagFilter<'_>,
    order: ListOrder,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Tag>> {
    // Without a `*`, like_pattern matches a prefix; this matches the name.
    let like = filter.pattern.map(|p| {
        let mut like = like_pattern(p);
        if !p.contains('*') {
            like.pop();
        }
        like
    });
    let query = match order {
        ListOrder::Count => select_tags!(
            "WHERE ($1::text[] IS NULL OR name = ANY($1)) AND ($2::text IS NULL OR name LIKE $2)
               AND (cardinality($3::int2[]) = 0 OR category_id = ANY($3)) AND (NOT $4 OR post_count > 0)
             ORDER BY post_count DESC, id OFFSET $5 LIMIT $6"
        ),
        ListOrder::Name => select_tags!(
            "WHERE ($1::text[] IS NULL OR name = ANY($1)) AND ($2::text IS NULL OR name LIKE $2)
               AND (cardinality($3::int2[]) = 0 OR category_id = ANY($3)) AND (NOT $4 OR post_count > 0)
             ORDER BY name OFFSET $5 LIMIT $6"
        ),
        ListOrder::Newest => select_tags!(
            "WHERE ($1::text[] IS NULL OR name = ANY($1)) AND ($2::text IS NULL OR name LIKE $2)
               AND (cardinality($3::int2[]) = 0 OR category_id = ANY($3)) AND (NOT $4 OR post_count > 0)
             ORDER BY id DESC OFFSET $5 LIMIT $6"
        ),
    };
    sqlx::query_as(query)
        .bind(filter.names)
        .bind(like)
        .bind(filter.categories)
        .bind(filter.used_only)
        .bind(offset)
        .bind(limit)
        .fetch_all(db)
        .await
}

/// How many of `post_ids` carry each of their tags, most first, at most
/// `limit` tags.
pub async fn counts_among(
    db: impl PgExecutor<'_>,
    post_ids: &[i64],
    limit: i64,
) -> sqlx::Result<Vec<(i32, i64)>> {
    sqlx::query_as(
        "SELECT tag_id, count(*) AS n FROM posts, unnest(tag_ids) AS tag_id
         WHERE id = ANY($1) GROUP BY tag_id ORDER BY n DESC, tag_id LIMIT $2",
    )
    .bind(post_ids)
    .bind(limit)
    .fetch_all(db)
    .await
}

/// A tag suggested while typing.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Suggestion {
    pub name: String,
    pub category_id: i16,
    pub post_count: i32,
    /// The alias typed, when the suggestion is its target.
    pub antecedent: Option<String>,
}

/// Up to `limit` used tags for `prefix`, most used first: tags starting
/// with it, tags that aliases starting with it point to, and when those
/// are few, similarly spelled tags.
pub async fn autocomplete(db: &PgPool, prefix: &str, limit: i64) -> sqlx::Result<Vec<Suggestion>> {
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    // A range rather than LIKE, so the index is used even in generic
    // (prepared) plans.
    let end = prefix_end(prefix);
    let mut found: Vec<Suggestion> = sqlx::query_as(
        "(SELECT name, category_id, post_count, NULL::text AS antecedent FROM tags
          WHERE name >= $1 AND ($2::text IS NULL OR name < $2) AND post_count > 0
          ORDER BY post_count DESC, name LIMIT $3)
         UNION ALL
         (SELECT t.name, t.category_id, t.post_count, r.antecedent_name::text
          FROM tag_relations r JOIN tags t ON t.name = r.consequent_name
          WHERE r.kind = 'alias' AND r.status = 'active'
            AND r.antecedent_name >= $1 AND ($2::text IS NULL OR r.antecedent_name < $2)
            AND t.post_count > 0
          ORDER BY t.post_count DESC, r.antecedent_name LIMIT $3)",
    )
    .bind(prefix)
    .bind(end)
    .bind(limit)
    .fetch_all(db)
    .await?;
    found.sort_by(|a, b| {
        b.post_count
            .cmp(&a.post_count)
            .then(a.antecedent.is_some().cmp(&b.antecedent.is_some()))
    });
    let mut seen = std::collections::HashSet::new();
    found.retain(|s| seen.insert(s.name.clone()));
    found.truncate(usize::try_from(limit).unwrap_or(0));

    if found.len() < 3 && prefix.chars().count() >= 3 {
        let similar: Vec<Suggestion> = sqlx::query_as(
            "SELECT name, category_id, post_count, NULL::text AS antecedent FROM tags
             WHERE name % $1 AND post_count > 0
             ORDER BY similarity(name, $1) DESC, post_count DESC LIMIT $2",
        )
        .bind(prefix)
        .bind(limit)
        .fetch_all(db)
        .await?;
        for suggestion in similar {
            if found.len() as i64 >= limit {
                break;
            }
            if seen.insert(suggestion.name.clone()) {
                found.push(suggestion);
            }
        }
    }
    Ok(found)
}

/// The smallest string above every string starting with `prefix` (in
/// the C collation tag names use), or `None` if there is none.
fn prefix_end(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        // The next code point, skipping the surrogate gap.
        let next = match u32::from(last) + 1 {
            0xD800 => Some('\u{E000}'),
            n => char::from_u32(n),
        };
        if let Some(next) = next {
            chars.push(next);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

/// A `LIKE` pattern for a tag name pattern: `*` matches anything, other
/// characters (including `_` and `%`) only themselves. Without a `*`, the
/// pattern is a prefix.
pub fn like_pattern(pattern: &str) -> String {
    let mut like = String::with_capacity(pattern.len() + 2);
    for c in pattern.chars() {
        match c {
            '*' => like.push('%'),
            '%' | '_' | '\\' => {
                like.push('\\');
                like.push(c);
            }
            c => like.push(c),
        }
    }
    if !pattern.contains('*') {
        like.push('%');
    }
    like
}

/// Recomputes every tag's post count from the posts, returning how many
/// were wrong. Blocks post edits while it runs (a full scan of `posts`), so
/// counts can't drift during the recount.
pub async fn recount(db: &PgPool) -> sqlx::Result<u64> {
    let mut tx = db.begin().await?;
    sqlx::query("LOCK TABLE posts IN SHARE MODE")
        .execute(&mut *tx)
        .await?;
    let fixed = sqlx::query(
        "WITH counts AS (
             SELECT tag_id, count(*)::integer AS n
             FROM posts, unnest(tag_ids) AS tag_id
             WHERE status IN ('active', 'flagged')
             GROUP BY tag_id
         )
         UPDATE tags SET post_count = coalesce(counts.n, 0)
         FROM tags AS t LEFT JOIN counts ON counts.tag_id = t.id
         WHERE tags.id = t.id AND tags.post_count IS DISTINCT FROM coalesce(counts.n, 0)",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(fixed)
}

#[cfg(test)]
pub(crate) mod tests {
    use sqlx::PgPool;

    use super::*;

    /// Creates tags by name, returning their ids in the same order.
    pub(crate) async fn create(pool: &PgPool, names: &[&str]) -> Vec<i32> {
        let mut ids = Vec::new();
        for name in names {
            ids.push(
                sqlx::query_scalar("INSERT INTO tags (name) VALUES ($1) RETURNING id")
                    .bind(name)
                    .fetch_one(pool)
                    .await
                    .unwrap(),
            );
        }
        ids
    }

    async fn counts(pool: &PgPool, ids: &[i32]) -> Vec<i32> {
        let mut out = Vec::new();
        for id in ids {
            out.push(
                sqlx::query_scalar("SELECT post_count FROM tags WHERE id = $1")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .unwrap(),
            );
        }
        out
    }

    async fn post(pool: &PgPool, status: &str, tags: &[i32]) -> i64 {
        let mut tags = tags.to_vec();
        tags.sort_unstable();
        sqlx::query_scalar(
            "INSERT INTO posts (rating, status, tag_ids) VALUES ('g', $1, $2) RETURNING id",
        )
        .bind(status)
        .bind(tags)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn default_categories(pool: PgPool) {
        let names: Vec<String> = categories(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(
            names,
            ["artist", "copyright", "character", "general", "meta"]
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn counts_follow_tag_and_status_changes(pool: PgPool) {
        let [a, b, c] = create(&pool, &["a", "b", "c"]).await[..] else {
            unreachable!()
        };
        let p1 = post(&pool, "active", &[a, b]).await;
        let p2 = post(&pool, "flagged", &[a]).await;
        post(&pool, "pending", &[a, b, c]).await;
        assert_eq!(counts(&pool, &[a, b, c]).await, [2, 1, 0]);

        sqlx::query("UPDATE posts SET tag_ids = $2 WHERE id = $1")
            .bind(p1)
            .bind(vec![b, c])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, &[a, b, c]).await, [1, 1, 1]);

        // Deleting a post (soft or hard) removes its tags from the counts;
        // restoring adds them back.
        for status in ["deleted", "active"] {
            sqlx::query("UPDATE posts SET status = $2 WHERE id = $1")
                .bind(p1)
                .bind(status)
                .execute(&pool)
                .await
                .unwrap();
        }
        assert_eq!(counts(&pool, &[a, b, c]).await, [1, 1, 1]);
        sqlx::query("UPDATE posts SET status = 'deleted' WHERE id = $1")
            .bind(p1)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM posts WHERE id = $1")
            .bind(p2)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, &[a, b, c]).await, [0, 0, 0]);

        // Unrelated edits don't touch tags.
        sqlx::query("UPDATE posts SET score = 5 WHERE id = $1")
            .bind(p1)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(counts(&pool, &[a, b, c]).await, [0, 0, 0]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn concurrent_tagging_keeps_exact_counts(pool: PgPool) {
        let ids = create(&pool, &["a", "b", "c", "d"]).await;
        let tasks: Vec<_> = (0..16)
            .map(|task| {
                let (pool, ids) = (pool.clone(), ids.clone());
                tokio::spawn(async move {
                    for i in 0..10 {
                        // Overlapping subsets, so transactions contend for
                        // the same tag rows.
                        let subset: Vec<i32> = ids
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| (task + i + j) % 3 != 0)
                            .map(|(_, id)| *id)
                            .collect();
                        post(&pool, "active", &subset).await;
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        let before = counts(&pool, &ids).await;
        assert_eq!(recount(&pool).await.unwrap(), 0, "{before:?}");
        assert!(before.iter().all(|&n| n > 0));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn tag_arrays_must_be_sorted_and_unique(pool: PgPool) {
        for bad in [vec![2, 1], vec![1, 1]] {
            let result = sqlx::query("INSERT INTO posts (rating, tag_ids) VALUES ('g', $1)")
                .bind(bad)
                .execute(&pool)
                .await;
            assert!(result.is_err());
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn recount_repairs_drift(pool: PgPool) {
        let ids = create(&pool, &["a", "b"]).await;
        post(&pool, "active", &ids).await;
        post(&pool, "active", &ids[..1]).await;
        sqlx::query("UPDATE tags SET post_count = 99")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(recount(&pool).await.unwrap(), 2);
        assert_eq!(counts(&pool, &ids).await, [2, 1]);
        assert_eq!(recount(&pool).await.unwrap(), 0);
    }

    #[test]
    fn prefix_ends() {
        assert_eq!(prefix_end("abc").as_deref(), Some("abd"));
        assert_eq!(prefix_end("a_").as_deref(), Some("a`"));
        assert_eq!(prefix_end("\u{D7FF}").as_deref(), Some("\u{E000}"));
        assert_eq!(prefix_end("a\u{10FFFF}").as_deref(), Some("b"));
        assert_eq!(prefix_end("\u{10FFFF}"), None);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn autocompletes(pool: PgPool) {
        let ids = create(
            &pool,
            &[
                "long_hair",
                "long_sleeves",
                "longcat",
                "lone",
                "unused_long",
            ],
        )
        .await;
        post(&pool, "active", &ids[..4]).await;
        post(&pool, "active", &ids[..2]).await;
        post(&pool, "active", &ids[1..2]).await;
        sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('alias', 'longer_hair', 'long_hair', 'active'),
                    ('alias', 'lo_old', 'gone', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let names = |found: Vec<Suggestion>| {
            found
                .into_iter()
                .map(|s| match s.antecedent {
                    Some(a) => format!("{a}→{}", s.name),
                    None => s.name,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(autocomplete(&pool, "long", 10).await.unwrap()),
            ["long_sleeves", "long_hair", "longcat"]
        );
        // Aliases lead to their target; with few matches, similar
        // spellings follow.
        let found = names(autocomplete(&pool, "longer", 10).await.unwrap());
        assert_eq!(found[0], "longer_hair→long_hair");
        assert_eq!(found.iter().filter(|n| n.contains("long_hair")).count(), 1);
        assert_eq!(
            names(autocomplete(&pool, "lo", 2).await.unwrap()),
            ["long_sleeves", "long_hair"]
        );
        // Few prefix matches: similar spellings help.
        assert!(
            names(autocomplete(&pool, "lnog_hair", 10).await.unwrap())
                .contains(&"long_hair".to_owned())
        );
        assert!(autocomplete(&pool, "", 10).await.unwrap().is_empty());
    }

    #[test]
    fn like_patterns() {
        assert_eq!(like_pattern("long_ha"), "long\\_ha%");
        assert_eq!(like_pattern("*_hair"), "%\\_hair");
        assert_eq!(like_pattern("100%"), "100\\%%");
        assert_eq!(like_pattern(""), "%");
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn ensure_creates_and_categorizes(pool: PgPool) {
        let existing = create(&pool, &["used", "unused"]).await;
        post(&pool, "active", &existing[..1]).await;
        let mut conn = pool.acquire().await.unwrap();
        let wanted = [
            WantedTag {
                name: "used",
                category_id: Some(1),
            },
            WantedTag {
                name: "unused",
                category_id: Some(1),
            },
            WantedTag {
                name: "new",
                category_id: Some(4),
            },
            WantedTag {
                name: "plain",
                category_id: None,
            },
        ];
        let mut tags = ensure(&mut conn, &wanted, false).await.unwrap();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        let summary: Vec<(&str, i16)> = tags
            .iter()
            .map(|t| (t.name.as_str(), t.category_id))
            .collect();
        // A used tag keeps its category unless recategorising is allowed.
        assert_eq!(
            summary,
            [("new", 4), ("plain", 0), ("unused", 1), ("used", 0)]
        );
        let tags = ensure(&mut conn, &wanted[..1], true).await.unwrap();
        assert_eq!(tags[0].category_id, 1);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn lists_by_pattern_and_order(pool: PgPool) {
        let ids = create(&pool, &["long_hair", "longhair", "short_hair", "hat"]).await;
        post(&pool, "active", &ids[2..]).await;
        post(&pool, "active", &ids[2..3]).await;
        let names = |tags: Vec<Tag>| tags.into_iter().map(|t| t.name).collect::<Vec<_>>();
        let page = |pattern: &'static str, order| {
            let pool = pool.clone();
            async move { names(list(&pool, pattern, None, order, 0, 10).await.unwrap()) }
        };
        assert_eq!(page("long_", ListOrder::Name).await, ["long_hair"]);
        assert_eq!(
            page("*_hair", ListOrder::Count).await,
            ["short_hair", "long_hair"]
        );
        assert_eq!(
            page("", ListOrder::Newest).await,
            ["hat", "short_hair", "longhair", "long_hair"]
        );
        assert_eq!(
            names(
                list(&pool, "", Some(4), ListOrder::Count, 0, 10)
                    .await
                    .unwrap()
            ),
            Vec::<String>::new()
        );
        assert!(update(&pool, ids[3], 4, true).await.unwrap());
        let hat = by_id(&pool, ids[3]).await.unwrap().unwrap();
        assert_eq!((hat.category_id, hat.is_deprecated), (4, true));
        assert!(!update(&pool, 12345, 0, false).await.unwrap());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn lookups(pool: PgPool) {
        let ids = create(&pool, &["common", "rare"]).await;
        post(&pool, "active", &ids).await;
        post(&pool, "active", &ids[..1]).await;
        assert_eq!(by_name(&pool, "rare").await.unwrap().unwrap().id, ids[1]);
        assert!(by_name(&pool, "missing").await.unwrap().is_none());
        let found = by_names(&pool, &["rare", "missing", "common"])
            .await
            .unwrap();
        assert_eq!(found.len(), 2);
        let ordered: Vec<String> = by_ids(&pool, &[ids[1], ids[0]])
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(ordered, ["common", "rare"]);
    }
}
