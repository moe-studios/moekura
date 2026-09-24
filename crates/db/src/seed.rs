//! Synthetic posts for load testing (`uwubooru admin seed`).
//!
//! Everything is generated inside PostgreSQL with set-based statements, so
//! millions of posts take minutes. Tags follow a Zipf distribution: a tag's
//! rank is drawn log-uniformly, so the probability of rank `r` falls as
//! `1/r`, like real boorus where a handful of tags are on most posts and
//! most tags are on a few. Seeded posts point at a placeholder file that
//! doesn't exist; they're for measuring the database, not for looking at.

use sqlx::{PgPool, Postgres, Transaction};

/// Seeded accounts are named with this prefix, which is how later runs
/// tell seeded posts from real ones.
pub const USER_PREFIX: &str = "seed_user_";

/// The storage key every seeded post's file points at.
pub const PLACEHOLDER_KEY: &str =
    "original/00/00/0000000000000000000000000000000000000000000000000000000000000000.png";

/// Words tag names are made of, so that wildcard and prefix searches have
/// realistic groups to match (`long_*`, `*_hair`).
const WORDS: &[&str] = &[
    "red",
    "blue",
    "green",
    "black",
    "white",
    "long",
    "short",
    "open",
    "closed",
    "small",
    "large",
    "hair",
    "eyes",
    "mouth",
    "hat",
    "dress",
    "shirt",
    "skirt",
    "shoes",
    "gloves",
    "ribbon",
    "bow",
    "sky",
    "cloud",
    "tree",
    "flower",
    "water",
    "night",
    "day",
    "city",
    "room",
    "window",
    "cat",
    "dog",
    "bird",
    "fish",
    "sword",
    "book",
    "cup",
    "chair",
    "bed",
    "car",
    "train",
    "star",
    "moon",
    "sun",
    "rain",
    "snow",
    "fire",
    "smile",
    "blush",
    "tears",
    "hands",
    "arms",
    "legs",
    "back",
    "side",
    "front",
    "view",
    "solo",
    "group",
    "holding",
    "sitting",
    "standing",
    "lying",
    "looking",
    "walking",
    "running",
    "outdoors",
    "indoors",
    "simple",
    "detailed",
    "background",
    "light",
    "shadow",
    "gradient",
    "border",
    "sketch",
    "painting",
    "comic",
    "text",
    "sign",
    "silver",
    "golden",
    "purple",
    "pink",
    "orange",
    "yellow",
    "brown",
    "grey",
    "striped",
    "dotted",
    "plaid",
    "frilled",
    "tied",
    "loose",
    "wet",
    "dry",
    "hot",
    "cold",
    "old",
    "new",
];

/// What to seed.
#[derive(Debug, Clone)]
pub struct Options {
    pub posts: u64,
    /// Distinct tags; defaults to one per 20 posts, at least 1,000.
    pub tags: Option<u32>,
    /// Makes the data reproducible: the same seed and sizes give the same
    /// posts.
    pub seed: u32,
    pub batch_size: u64,
}

impl Options {
    pub fn tag_count(&self) -> u32 {
        self.tags
            .unwrap_or_else(|| u32::try_from(self.posts / 20).unwrap_or(u32::MAX).max(1000))
    }
}

/// What batches need: the seeded users and tags, by rank.
#[derive(Debug, Clone)]
pub struct Plan {
    pub user_ids: Vec<i64>,
    /// Tag ids, most common first.
    pub tag_ids: Vec<i32>,
    /// Post number of the first post this run adds, for timestamps.
    pub first: u64,
    pub total: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error(
        "the database has {0} posts that weren't seeded; seed an empty or test database, or pass --force"
    )]
    RealPosts(i64),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// The name of the tag at `rank` (1-based).
pub fn tag_name(rank: u32) -> String {
    let n = WORDS.len();
    let i = rank as usize - 1;
    let base = format!("{}_{}", WORDS[i % n], WORDS[(i / n) % n]);
    match i / (n * n) {
        0 => base,
        round => format!("{base}_{round}"),
    }
}

/// The category of the tag at `rank`: the most common tags are general,
/// artists and characters fill the long tail.
fn category(rank: u32) -> i16 {
    const GENERAL: i16 = 0;
    const ARTIST: i16 = 1;
    const COPYRIGHT: i16 = 3;
    const CHARACTER: i16 = 4;
    const META: i16 = 5;
    match rank {
        0..=100 => GENERAL,
        r if r % 97 == 0 => META,
        r if r % 50 == 0 => COPYRIGHT,
        r if r % 10 == 3 => CHARACTER,
        r if r % 5 == 1 => ARTIST,
        _ => GENERAL,
    }
}

/// Makes sure seeding won't mix with real posts, then creates the users
/// and tags.
pub async fn prepare(db: &PgPool, options: &Options, force: bool) -> Result<Plan, SeedError> {
    let real: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM posts p
         WHERE NOT EXISTS (SELECT 1 FROM users u WHERE u.id = p.uploader_id AND u.name LIKE $1)",
    )
    .bind(format!("{USER_PREFIX}%"))
    .fetch_one(db)
    .await?;
    if real > 0 && !force {
        return Err(SeedError::RealPosts(real));
    }
    let seeded: i64 = sqlx::query_scalar("SELECT count(*) FROM posts")
        .fetch_one(db)
        .await?;

    let users = (options.posts / 500).clamp(10, 100_000) as i64;
    let user_ids: Vec<i64> = sqlx::query_scalar(
        "WITH made AS (
             INSERT INTO users (name, role_id)
             SELECT $1 || n, (SELECT id FROM roles WHERE system_key = 'member')
             FROM generate_series(1, $2) n
             ON CONFLICT DO NOTHING
         )
         SELECT id FROM users WHERE name LIKE $1 || '%' ORDER BY id",
    )
    .bind(USER_PREFIX)
    .bind(users)
    .fetch_all(db)
    .await?;
    // The CTE's rows aren't visible to the same statement's SELECT on a
    // first run.
    let user_ids = if user_ids.is_empty() {
        sqlx::query_scalar("SELECT id FROM users WHERE name LIKE $1 || '%' ORDER BY id")
            .bind(USER_PREFIX)
            .fetch_all(db)
            .await?
    } else {
        user_ids
    };

    let count = options.tag_count();
    let names: Vec<String> = (1..=count).map(tag_name).collect();
    let categories: Vec<i16> = (1..=count).map(category).collect();
    sqlx::query(
        "INSERT INTO tags (name, category_id)
         SELECT * FROM unnest($1::text[], $2::int2[])
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(&names)
    .bind(&categories)
    .execute(db)
    .await?;
    let tag_ids: Vec<i32> = sqlx::query_scalar(
        "SELECT t.id FROM unnest($1::text[]) WITH ORDINALITY AS n (name, rank)
         JOIN tags t ON t.name = n.name
         ORDER BY n.rank",
    )
    .bind(&names)
    .fetch_all(db)
    .await?;

    Ok(Plan {
        user_ids,
        tag_ids,
        first: seeded as u64,
        total: seeded as u64 + options.posts,
    })
}

/// Adds posts `from..to` (0-based within this run) with their media rows.
pub async fn batch(
    db: &PgPool,
    plan: &Plan,
    options: &Options,
    from: u64,
    to: u64,
) -> sqlx::Result<()> {
    let mut tx: Transaction<'_, Postgres> = db.begin().await?;
    // Reproducible: the same batch always draws the same numbers.
    let seed =
        f64::from(options.seed.wrapping_mul(2_654_435_761) ^ from as u32) / f64::from(u32::MAX);
    sqlx::query("SELECT setseed($1 * 2 - 1)")
        .bind(seed)
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL max_parallel_workers_per_gather = 0")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        r#"
        WITH drawn AS (
            SELECT g, random() AS status_roll, random() AS type_roll, random() AS size_roll,
                   random() AS score_roll, random() AS fav_roll
            FROM generate_series($3::bigint, $4::bigint - 1) g
        ), made AS (
            INSERT INTO posts (uploader_id, rating, status, score, fav_count, tag_ids, created_at, updated_at)
            SELECT
                -- A few prolific uploaders, like real sites.
                ($1::bigint[])[least(cardinality($1::bigint[]),
                    floor(exp(random() * ln(cardinality($1::bigint[]) + 1)))::int)],
                (ARRAY['g','g','g','g','s','s','s','q','q','e'])[1 + floor(random() * 10)::int],
                CASE WHEN status_roll < 0.005 THEN 'deleted'
                     WHEN status_roll < 0.008 THEN 'pending'
                     WHEN status_roll < 0.010 THEN 'flagged'
                     ELSE 'active' END,
                floor(-ln(1 - score_roll) * 8)::int - 2,
                floor(-ln(1 - fav_roll) * 5)::int,
                ARRAY(
                    SELECT DISTINCT ($2::int[])[least(cardinality($2::int[]),
                        floor(exp(random() * ln(cardinality($2::int[]) + 1)))::int)]
                    -- Correlated with the post, so it's drawn per post.
                    FROM generate_series(1, 8 + (g % 30)::int)
                    ORDER BY 1
                ),
                now() - (($6::bigint - $5::bigint - g) * interval '1 minute'),
                now() - (($6::bigint - $5::bigint - g) * interval '1 minute')
            FROM drawn
            ORDER BY g
            RETURNING id
        ), numbered AS (
            SELECT id, row_number() OVER (ORDER BY id) AS n FROM made
        ), media AS (
            SELECT m.id, d.type_roll, d.size_roll,
                   CASE WHEN d.type_roll < 0.55 THEN 'jpeg'
                        WHEN d.type_roll < 0.85 THEN 'png'
                        WHEN d.type_roll < 0.88 THEN 'gif'
                        WHEN d.type_roll < 0.93 THEN 'webp'
                        WHEN d.type_roll < 0.98 THEN 'mp4'
                        ELSE 'webm' END AS media_type,
                   500 + floor(d.size_roll * 3500)::int AS width,
                   500 + floor(random() * 3500)::int AS height
            FROM numbered m JOIN drawn d ON d.g = $3::bigint + m.n - 1
        )
        INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, duration_ms,
                                  frames, has_audio, file_size, storage_key, processed_at)
        SELECT id,
               decode(md5(random()::text) || md5(random()::text), 'hex'),
               decode(md5(random()::text), 'hex'),
               media_type, width, height,
               CASE WHEN media_type IN ('mp4', 'webm') THEN 1000 + floor(random() * 120000)::int END,
               CASE WHEN media_type IN ('mp4', 'webm', 'gif') THEN 24 ELSE 1 END,
               media_type IN ('mp4', 'webm') AND random() < 0.5,
               greatest(1, (width::bigint * height * (0.1 + random() * 0.4))::bigint),
               $7, now()
        FROM media
        "#,
    )
    .bind(&plan.user_ids)
    .bind(&plan.tag_ids)
    .bind(from as i64)
    .bind(to as i64)
    .bind(plan.first as i64)
    .bind(plan.total as i64)
    .bind(PLACEHOLDER_KEY)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Adds aliases and implications among the seeded tags, applies the
/// implications, and refreshes planner statistics.
pub async fn finish(db: &PgPool, plan: &Plan) -> sqlx::Result<()> {
    let creator = plan.user_ids.first().copied();
    // Aliases: an old spelling for one in 50 of the 5,000 most used tags.
    sqlx::query(
        "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status, creator_id)
         SELECT 'alias', 'alt_' || t.name, t.name, 'active', $2
         FROM unnest($1::int[]) WITH ORDINALITY AS r (id, rank)
         JOIN tags t ON t.id = r.id
         WHERE r.rank <= 5000 AND r.rank % 50 = 7
         ON CONFLICT DO NOTHING",
    )
    .bind(&plan.tag_ids)
    .bind(creator)
    .execute(db)
    .await?;
    // Implications: characters imply a copyright, as on real sites.
    let pairs: Vec<(i32, i32, String, String)> = sqlx::query_as(
        "WITH ranked AS (
             SELECT t.id, t.name, t.category_id, r.rank
             FROM unnest($1::int[]) WITH ORDINALITY AS r (id, rank) JOIN tags t ON t.id = r.id
             WHERE r.rank <= 3000
         ), characters AS (
             SELECT *, row_number() OVER (ORDER BY rank) AS n FROM ranked WHERE category_id = 4
         ), copyrights AS (
             SELECT *, row_number() OVER (ORDER BY rank) AS n FROM ranked WHERE category_id = 3
         )
         SELECT c.id, p.id, c.name, p.name
         FROM characters c
         JOIN copyrights p ON p.n = 1 + (c.n - 1) % (SELECT greatest(count(*), 1) FROM copyrights)
         WHERE c.n <= 20",
    )
    .bind(&plan.tag_ids)
    .fetch_all(db)
    .await?;
    for (antecedent, consequent, antecedent_name, consequent_name) in pairs {
        let inserted = sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status, creator_id)
             VALUES ('implication', $1, $2, 'active', $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(&antecedent_name)
        .bind(&consequent_name)
        .bind(creator)
        .execute(db)
        .await?;
        if inserted.rows_affected() == 0 {
            continue;
        }
        sqlx::query(
            "UPDATE posts SET tag_ids = sort(uniq(tag_ids || $2::int))
             WHERE tag_ids @> ARRAY[$1::int] AND NOT tag_ids @> ARRAY[$2::int]",
        )
        .bind(antecedent)
        .bind(consequent)
        .execute(db)
        .await?;
    }
    sqlx::query("ANALYZE posts, tags, media_assets, users, tag_relations")
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tags_uniquely() {
        let n = WORDS.len() as u32;
        let names: std::collections::HashSet<String> = (1..=n * n + 10).map(tag_name).collect();
        assert_eq!(names.len() as u32, n * n + 10);
        assert_eq!(tag_name(1), "red_red");
        assert_eq!(tag_name(2), "blue_red");
        assert!(tag_name(n * n + 1).ends_with("_1"));
        for rank in [1, 1000, n * n + 5] {
            assert!(
                uwu_core::tags::TagName::parse(&tag_name(rank)).is_ok(),
                "{}",
                tag_name(rank)
            );
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn seeds_consistent_posts(pool: PgPool) {
        let options = Options {
            posts: 3000,
            tags: Some(500),
            seed: 7,
            batch_size: 1000,
        };
        let plan = prepare(&pool, &options, false).await.unwrap();
        assert_eq!(plan.tag_ids.len(), 500);
        for from in (0..3000).step_by(1000) {
            batch(&pool, &plan, &options, from, from + 1000)
                .await
                .unwrap();
        }
        finish(&pool, &plan).await.unwrap();

        let (posts, assets, versions): (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM posts), (SELECT count(*) FROM media_assets),
                    (SELECT count(*) FROM post_versions)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((posts, assets), (3000, 3000));
        assert!(versions >= 3000);

        // Tag counts match the visible posts, and follow the Zipf shape.
        let wrong: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tags t
             WHERE post_count <> (SELECT count(*) FROM posts
                                  WHERE tag_ids @> ARRAY[t.id] AND status IN ('active', 'flagged'))",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(wrong, 0);
        let counts: Vec<i32> = sqlx::query_scalar(
            "SELECT post_count FROM tags WHERE id = ANY($1) ORDER BY post_count DESC",
        )
        .bind(&plan.tag_ids)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            counts[0] > 1500,
            "the top tag is on most posts: {}",
            counts[0]
        );
        assert!(counts[250] < counts[0] / 20, "a long tail: {}", counts[250]);

        let implied: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM tag_relations r
             JOIN tags a ON a.name = r.antecedent_name JOIN tags c ON c.name = r.consequent_name
             JOIN posts p ON p.tag_ids @> ARRAY[a.id] AND NOT p.tag_ids @> ARRAY[c.id]
             WHERE r.kind = 'implication'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(implied, 0, "implications are applied");

        // Seeding more is fine; real posts stop it.
        let again = prepare(&pool, &options, false).await.unwrap();
        assert_eq!(again.first, 3000);
        sqlx::query("INSERT INTO posts (rating) VALUES ('g')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            prepare(&pool, &options, false).await,
            Err(SeedError::RealPosts(1))
        ));
    }
}
