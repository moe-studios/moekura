//! `moekura admin bench-http`: times whole pages and API responses from a
//! running server, the way visitors and apps wait for them.
//!
//! What to load is picked from the database, like `admin bench` picks its
//! searches: the busiest post (comments, notes and a pool), common and rare
//! tags, the biggest pool. Requests are anonymous.

use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use futures_util::StreamExt;
use sqlx::PgPool;

/// A page or API call to time.
#[derive(Debug, Clone)]
pub struct Target {
    pub name: &'static str,
    /// Paths and queries, e.g. `/posts?tags=red`, loaded in turn: several
    /// posts or tags of a kind, so not every request finds what the last
    /// one left in caches.
    pub paths: Vec<String>,
}

/// How a target did.
struct Measured {
    p50: Duration,
    p95: Duration,
    max: Duration,
    /// Mean body size in bytes.
    bytes: u64,
    /// Responses that weren't 200 OK, with the first one's status.
    failed: Option<(usize, reqwest::StatusCode)>,
}

pub struct Options<'a> {
    /// The server, e.g. `http://localhost:8080`.
    pub url: &'a str,
    pub runs: usize,
    pub concurrency: usize,
    /// Only targets whose names contain this.
    pub only: Option<&'a str>,
    /// Fail if a p95 is above this.
    pub check: Option<Duration>,
}

fn encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// The targets for the data in `db`. Those whose ingredients are missing
/// (an empty database) are left out.
pub async fn targets(db: &PgPool) -> sqlx::Result<Vec<Target>> {
    let top: Vec<String> =
        sqlx::query_scalar("SELECT name FROM tags ORDER BY post_count DESC, id LIMIT 2")
            .fetch_all(db)
            .await?;
    let rare: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM tags WHERE post_count BETWEEN 20 AND 200 ORDER BY post_count DESC, id LIMIT 10",
    )
    .fetch_all(db)
    .await?;
    // The most commented posts that also have notes and are in a pool, so
    // their pages show everything; failing that, the most commented.
    let mut posts: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM posts p
         WHERE status = 'active' AND comment_count > 0 AND note_count > 0
           AND EXISTS (SELECT 1 FROM pool_posts pp WHERE pp.post_id = p.id)
         ORDER BY comment_count DESC, id LIMIT 10",
    )
    .fetch_all(db)
    .await?;
    if posts.is_empty() {
        posts = sqlx::query_scalar(
            "SELECT id FROM posts WHERE status = 'active' ORDER BY comment_count DESC, id LIMIT 10",
        )
        .fetch_all(db)
        .await?;
    }
    let pools: Vec<i32> = sqlx::query_scalar(
        "SELECT pp.pool_id FROM pool_posts pp JOIN pools p ON p.id = pp.pool_id AND NOT p.is_deleted
         GROUP BY pp.pool_id ORDER BY count(*) DESC, pp.pool_id LIMIT 10",
    )
    .fetch_all(db)
    .await?;

    let mut targets = Vec::new();
    let mut add = |name, paths: Vec<String>| {
        if !paths.is_empty() {
            targets.push(Target { name, paths });
        }
    };
    let each = |format: fn(&str) -> String, items: &[String]| -> Vec<String> {
        items.iter().map(|item| format(item)).collect()
    };
    let rare = each(encode, &rare);
    let posts: Vec<String> = posts.iter().map(i64::to_string).collect();
    add("front page", vec!["/".into()]);
    if let [t1, t2] = top.as_slice() {
        add("common tag", vec![format!("/posts?tags={}", encode(t1))]);
        add(
            "two common",
            vec![format!("/posts?tags={}", encode(&format!("{t1} {t2}")))],
        );
        add(
            "common, page 2",
            vec![format!("/posts?tags={}&page=2", encode(t1))],
        );
    }
    add("rare tag", each(|t| format!("/posts?tags={t}"), &rare));
    add("post page", each(|id| format!("/posts/{id}"), &posts));
    add(
        "pool page",
        pools.iter().map(|id| format!("/pools/{id}")).collect(),
    );
    add("comments", vec!["/comments".into()]);
    add("tag list", vec!["/tags".into()]);
    add("api posts", vec!["/api/v1/posts".into()]);
    if let Some(t1) = top.first() {
        add(
            "api posts, tag",
            vec![format!("/api/v1/posts?tags={}", encode(t1))],
        );
    }
    add("api post", each(|id| format!("/api/v1/posts/{id}"), &posts));
    add("danbooru posts", vec!["/posts.json".into()]);
    if let Some(t1) = top.first() {
        add(
            "danbooru posts, tag",
            vec![format!("/posts.json?tags={}", encode(t1))],
        );
    }
    add(
        "danbooru post",
        each(|id| format!("/posts/{id}.json"), &posts),
    );
    // What typing brings up: the first letters of popular tags.
    let mut prefixes: Vec<String> = top
        .iter()
        .chain(&rare)
        .filter_map(|t| t.get(..2))
        .map(encode)
        .collect();
    prefixes.dedup();
    add(
        "autocomplete",
        each(|p| format!("/tags/autocomplete?q={p}"), &prefixes),
    );
    add(
        "api autocomplete",
        each(|p| format!("/api/v1/tags/autocomplete?q={p}"), &prefixes),
    );
    add(
        "danbooru autocomplete",
        each(
            |p| format!("/autocomplete.json?search%5Bquery%5D={p}&search%5Btype%5D=tag_query"),
            &prefixes,
        ),
    );
    add("feed", vec!["/posts.atom".into()]);
    if let Some(t1) = top.first() {
        add(
            "feed, tag",
            vec![format!("/posts.atom?tags={}", encode(t1))],
        );
    }
    Ok(targets)
}

/// Loads each of `urls` once to warm up, then `runs` times between them,
/// `concurrency` at a time.
async fn measure(
    client: &reqwest::Client,
    urls: &[String],
    runs: usize,
    concurrency: usize,
) -> anyhow::Result<Measured> {
    let fetch = |url: &String| {
        let url = url.clone();
        async move {
            let started = Instant::now();
            let result = async {
                let response = client.get(&url).send().await?;
                let status = response.status();
                let body = response.bytes().await?;
                Ok::<_, reqwest::Error>((status, body.len() as u64))
            }
            .await
            .with_context(|| format!("could not load {url}"))?;
            anyhow::Ok((started.elapsed(), result.0, result.1))
        }
    };
    for url in urls {
        fetch(url).await?;
    }
    let results: Vec<_> = futures_util::stream::iter(0..runs.max(1))
        .map(|run| fetch(&urls[run % urls.len()]))
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await;
    let mut times = Vec::with_capacity(results.len());
    let mut bytes = 0;
    let mut failed: Option<(usize, reqwest::StatusCode)> = None;
    for result in results {
        let (time, status, size) = result?;
        times.push(time);
        bytes += size;
        if status != reqwest::StatusCode::OK {
            let (n, _) = failed.get_or_insert((0, status));
            *n += 1;
        }
    }
    times.sort();
    let at = |q: f64| times[((times.len() as f64 - 1.0) * q).round() as usize];
    Ok(Measured {
        p50: at(0.5),
        p95: at(0.95),
        max: *times.last().expect("ran at least once"),
        bytes: bytes / times.len() as u64,
        failed,
    })
}

pub async fn run(db: &PgPool, options: Options<'_>) -> anyhow::Result<()> {
    let base = options.url.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let targets: Vec<Target> = targets(db)
        .await?
        .into_iter()
        .filter(|t| options.only.is_none_or(|only| t.name.contains(only)))
        .collect();
    println!(
        "{base}: {} requests per target, {} at a time\n",
        options.runs, options.concurrency
    );
    println!(
        "{:<22} {:>8} {:>8} {:>8} {:>8}  first path",
        "target", "p50 ms", "p95 ms", "max ms", "KiB"
    );
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let mut problems = Vec::new();
    for target in &targets {
        let urls: Vec<String> = target.paths.iter().map(|p| format!("{base}{p}")).collect();
        let m = measure(&client, &urls, options.runs, options.concurrency).await?;
        println!(
            "{:<22} {:>8.1} {:>8.1} {:>8.1} {:>8.1}  {}",
            target.name,
            ms(m.p50),
            ms(m.p95),
            ms(m.max),
            m.bytes as f64 / 1024.0,
            target.paths[0],
        );
        if let Some((n, status)) = m.failed {
            problems.push(format!("{} answered {status} {n} time(s)", target.name));
        }
        if let Some(limit) = options.check.filter(|limit| m.p95 > *limit) {
            problems.push(format!(
                "{} took {:.0} ms (p95), over {:.0} ms",
                target.name,
                ms(m.p95),
                ms(limit)
            ));
        }
    }
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("{problem}");
        }
        bail!("{} target(s) failed", problems.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn leaves_out_what_the_database_lacks(pool: PgPool) {
        let names: Vec<&str> = targets(&pool)
            .await
            .unwrap()
            .iter()
            .map(|t| t.name)
            .collect();
        assert!(names.contains(&"front page"), "{names:?}");
        assert!(names.contains(&"feed"), "{names:?}");
        assert!(!names.contains(&"post page"), "{names:?}");
        assert!(!names.contains(&"rare tag"), "{names:?}");

        sqlx::query("INSERT INTO posts (rating) VALUES ('g')")
            .execute(&pool)
            .await
            .unwrap();
        let found = targets(&pool).await.unwrap();
        let post = found.iter().find(|t| t.name == "post page").unwrap();
        assert_eq!(post.paths.len(), 1);
        assert!(post.paths[0].starts_with("/posts/"), "{:?}", post.paths);
    }
}
