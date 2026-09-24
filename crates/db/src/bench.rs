//! A fixed suite of searches for measuring search at scale
//! (`moekura admin bench`), and the checks CI runs on their plans.
//!
//! The suite adapts to the data: it picks the most common tags, a mid-range
//! pair, a rare tag and so on from `tags`, so it means the same on any
//! database. Besides wall time, each case reports how many table and index
//! pages its queries touched. That number doesn't depend on the machine,
//! which makes it the thing to check in CI: a plan that regresses to
//! reading a whole table shows up however fast the runner is.

use std::time::{Duration, Instant};

use moekura_core::config::SearchConfig;
use moekura_core::posts::PostStatus;
use moekura_core::search::Query;
use serde_json::Value as Json;
use sqlx::PgPool;

use crate::posts::Visibility;
use crate::search::{Count, PageRef, Plan, SearchError};

/// One search in the suite.
#[derive(Debug, Clone)]
pub struct Case {
    pub name: &'static str,
    pub query: String,
    pub page: PageRef,
    /// Reading much of the posts table is expected: nothing indexes what
    /// it filters on, or it's a deep numbered page.
    pub may_scan: bool,
}

/// The tags and values the suite is built from.
async fn pick(db: &PgPool, sql: &'static str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(sql).fetch_optional(db).await
}

/// The suite for the data in `db`. Cases whose ingredients are missing
/// (an empty database) are left out.
pub async fn suite(db: &PgPool, config: &SearchConfig) -> sqlx::Result<Vec<Case>> {
    let top: Vec<String> =
        sqlx::query_scalar("SELECT name FROM tags ORDER BY post_count DESC, id LIMIT 3")
            .fetch_all(db)
            .await?;
    let mid: Vec<String> =
        sqlx::query_scalar("SELECT name FROM tags ORDER BY post_count DESC, id OFFSET 100 LIMIT 2")
            .fetch_all(db)
            .await?;
    let rare = pick(
        db,
        "SELECT name FROM tags WHERE post_count BETWEEN 20 AND 200 ORDER BY post_count DESC, id LIMIT 1",
    )
    .await?;
    let uploader = pick(
        db,
        "SELECT u.name::text FROM users u
         JOIN LATERAL (SELECT count(*) AS n FROM posts WHERE uploader_id = u.id) c ON true
         ORDER BY c.n DESC LIMIT 1",
    )
    .await?;
    let middle_id: Option<i64> = sqlx::query_scalar("SELECT (max(id) + min(id)) / 2 FROM posts")
        .fetch_one(db)
        .await?;
    let middle_year: Option<i32> = sqlx::query_scalar(
        "SELECT extract(year FROM created_at)::int FROM posts WHERE id >= (SELECT (max(id) + min(id)) / 2 FROM posts) ORDER BY id LIMIT 1",
    )
    .fetch_optional(db)
    .await?;

    let mut cases = vec![Case {
        name: "front page",
        query: String::new(),
        page: PageRef::default(),
        may_scan: false,
    }];
    let mut add = |name, query: String, page, may_scan| {
        cases.push(Case {
            name,
            query,
            page,
            may_scan,
        });
    };
    let first_word = |tag: &str| tag.split('_').next().unwrap_or(tag).to_owned();
    let last_word = |tag: &str| tag.rsplit('_').next().unwrap_or(tag).to_owned();
    if let [t1, t2, t3] = top.as_slice() {
        add("common tag", t1.clone(), PageRef::default(), false);
        add(
            "two common",
            format!("{t1} {t2}"),
            PageRef::default(),
            false,
        );
        add(
            "three common",
            format!("{t1} {t2} {t3}"),
            PageRef::default(),
            false,
        );
        add(
            "negated common",
            format!("{t2} -{t1}"),
            PageRef::default(),
            false,
        );
        add(
            "common, by score",
            format!("{t1} order:score"),
            PageRef::default(),
            false,
        );
        let deep = config.max_page.min(500);
        // Numbered pages read every post before them; that's why "next"
        // links switch to cursors.
        add("common, page 500", t1.clone(), PageRef::Number(deep), true);
        if let Some(id) = middle_id {
            add(
                "common, deep cursor",
                t1.clone(),
                PageRef::Before(id),
                false,
            );
        }
        if let Some(rare) = &rare {
            add(
                "common and rare",
                format!("{t1} {rare}"),
                PageRef::default(),
                false,
            );
        }
        add(
            "filetype and common",
            format!("filetype:mp4 {t2}"),
            PageRef::default(),
            false,
        );
    }
    if let Some(rare) = &rare {
        add("rare tag", rare.clone(), PageRef::default(), false);
        if let Some(id) = middle_id {
            add(
                "rare, deep cursor",
                rare.clone(),
                PageRef::Before(id),
                false,
            );
        }
    }
    if let [m1, m2] = mid.as_slice() {
        add(
            "or group",
            format!("~{m1} ~{m2}"),
            PageRef::default(),
            false,
        );
        add(
            "prefix wildcard",
            format!("{}_*", first_word(m1)),
            PageRef::default(),
            false,
        );
        add(
            "suffix wildcard",
            format!("*_{}", last_word(m2)),
            PageRef::default(),
            false,
        );
    }
    add(
        "rating and score",
        "rating:e score:>20".into(),
        PageRef::default(),
        false,
    );
    add(
        "by favorites",
        "order:favcount".into(),
        PageRef::default(),
        false,
    );
    if let Some(user) = uploader {
        add(
            "uploader",
            format!("user:{user}"),
            PageRef::default(),
            false,
        );
    }
    if let Some(year) = middle_year {
        add("year", format!("date:{year}"), PageRef::default(), false);
        if let Some(t2) = top.get(1) {
            add(
                "common and year",
                format!("{t2} date:{year}"),
                PageRef::default(),
                false,
            );
        }
        if let Some(id) = middle_id {
            add(
                "year, cursor",
                format!("date:{year}"),
                PageRef::Before(id),
                false,
            );
        }
    }
    // Nothing indexes tag counts.
    add("tag count", "tagcount:>35".into(), PageRef::default(), true);
    Ok(cases)
}

/// How a case did.
#[derive(Debug, Clone)]
pub struct Measured {
    pub case: Case,
    pub strategy: &'static str,
    pub count: Count,
    pub p50: Duration,
    pub p95: Duration,
    pub max: Duration,
    /// Pages the ids and count queries touched (shared buffers hit or
    /// read).
    pub pages: u64,
    /// Whether either query read the posts table sequentially.
    pub seq_scan: bool,
}

/// Anonymous visitors: what a public front page runs as.
fn visitor() -> Visibility {
    Visibility {
        statuses: vec![PostStatus::Active, PostStatus::Flagged],
        viewer: None,
    }
}

/// Pages a JSON plan's root touched.
fn pages(plan: &Json) -> u64 {
    let root = &plan[0]["Plan"];
    ["Shared Hit Blocks", "Shared Read Blocks"]
        .iter()
        .filter_map(|key| root[key].as_u64())
        .sum()
}

/// Whether any node in a JSON plan scans `posts` sequentially.
fn scans_posts(node: &Json) -> bool {
    let this = node["Node Type"] == "Seq Scan" && node["Relation Name"] == "posts";
    this || node["Plans"]
        .as_array()
        .is_some_and(|children| children.iter().any(scans_posts))
        || node[0]["Plan"].is_object() && scans_posts(&node[0]["Plan"])
}

/// Runs `case` once to warm up, then `runs` times, timing what a search
/// page does (ids and count), and explains it.
pub async fn measure(
    db: &PgPool,
    config: &SearchConfig,
    case: &Case,
    runs: usize,
) -> Result<Measured, SearchError> {
    let query = Query::parse(&case.query).map_err(|e| SearchError::Invalid(e.to_string()))?;
    let runs = runs.max(1);
    let mut times = Vec::with_capacity(runs);
    let mut last = None;
    for run in 0..=runs {
        // Everything a search page does: look up the tags, then fetch the
        // page and the count.
        let started = Instant::now();
        let plan = Plan::resolve(db, &query, &visitor(), config).await?;
        plan.ids(db, case.page).await?;
        let count = plan.count(db).await?;
        if run > 0 {
            times.push(started.elapsed());
        }
        last = Some((plan, count));
    }
    let (plan, count) = last.expect("ran at least once");
    times.sort();
    let at = |q: f64| times[((times.len() as f64 - 1.0) * q).round() as usize];
    let (ids_plan, count_plan) = plan.explain(db, case.page).await?;
    let plans: Vec<&Json> = [&ids_plan, &count_plan].into_iter().flatten().collect();
    Ok(Measured {
        strategy: plan.strategy_name(case.page),
        count,
        p50: at(0.5),
        p95: at(0.95),
        max: *times.last().unwrap_or(&Duration::ZERO),
        pages: plans.iter().map(|p| pages(p)).sum(),
        seq_scan: plans.iter().any(|p| scans_posts(p)),
        case: case.clone(),
    })
}

/// The posts table's size in pages.
pub async fn posts_pages(db: &PgPool) -> sqlx::Result<u64> {
    let pages: i64 = sqlx::query_scalar(
        "SELECT pg_relation_size('posts') / current_setting('block_size')::bigint",
    )
    .fetch_one(db)
    .await?;
    Ok(pages.max(0) as u64)
}

/// What's wrong with `measured`, if anything: a case that isn't expected
/// to scan read over half as many pages as the posts table has.
pub fn problem(measured: &Measured, table_pages: u64) -> Option<String> {
    if measured.case.may_scan {
        return None;
    }
    (measured.pages > table_pages / 2).then(|| {
        format!(
            "{} (`{}`) touched {} pages; the posts table has {}{}",
            measured.case.name,
            measured.case.query,
            measured.pages,
            table_pages,
            if measured.seq_scan {
                ", and it scanned posts"
            } else {
                ""
            }
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed;

    /// Seeds 200,000 posts, so it's left out of normal runs; CI runs it
    /// with `--ignored`.
    #[sqlx::test(migrator = "crate::MIGRATOR")]
    #[ignore = "seeds 200,000 posts; run with --ignored"]
    async fn search_plans_stay_selective(pool: PgPool) {
        let options = seed::Options {
            posts: 200_000,
            tags: None,
            seed: 1,
            batch_size: 50_000,
        };
        let plan = seed::prepare(&pool, &options, false).await.unwrap();
        for from in (0..options.posts).step_by(options.batch_size as usize) {
            seed::batch(&pool, &plan, &options, from, from + options.batch_size)
                .await
                .unwrap();
        }
        seed::finish(&pool, &plan).await.unwrap();

        // A small database with a small cost limit behaves like a big
        // one with the default: queries that would read everything
        // estimate instead.
        let config = SearchConfig {
            count_cost_limit: 1000,
            ..SearchConfig::default()
        };
        let table = posts_pages(&pool).await.unwrap();
        let cases = suite(&pool, &config).await.unwrap();
        assert!(cases.len() >= 18, "{cases:?}");
        let mut problems = Vec::new();
        for case in &cases {
            let measured = measure(&pool, &config, case, 1).await.unwrap();
            eprintln!(
                "{:<22} {:>8} pages  {:<8} {}",
                case.name, measured.pages, measured.strategy, case.query
            );
            problems.extend(problem(&measured, table));
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
