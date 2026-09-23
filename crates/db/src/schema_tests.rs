//! Tests that pin down schema properties the application relies on but
//! that no single query module owns.

use serde_json::Value;
use sqlx::PgPool;

/// Index names used by the plan for `query`, with sequential scans off so
/// a small test table still shows which indexes are usable.
async fn plan_indexes(pool: &PgPool, query: &'static str) -> Vec<String> {
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "EXPLAIN (FORMAT JSON) {query}"
    )))
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    let mut names = Vec::new();
    collect_index_names(&plan, &mut names);
    names
}

fn collect_index_names(node: &Value, names: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(name)) = map.get("Index Name") {
                names.push(name.clone());
            }
            map.values().for_each(|v| collect_index_names(v, names));
        }
        Value::Array(items) => items.iter().for_each(|v| collect_index_names(v, names)),
        _ => {}
    }
}

async fn seed_posts(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO posts (rating, tag_ids)
         SELECT 'g', ARRAY[i % 50, i % 7 + 100, 1000 + i]::int[]
         FROM generate_series(1, 2000) AS i",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE posts").execute(pool).await.unwrap();
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn tag_queries_use_the_gin_index(pool: PgPool) {
    seed_posts(&pool).await;
    for query in [
        "SELECT id FROM posts WHERE tag_ids @> '{5, 101}'",
        "SELECT id FROM posts WHERE tag_ids && '{5, 6}'",
    ] {
        let indexes = plan_indexes(&pool, query).await;
        assert!(
            indexes.contains(&"posts_tag_ids_idx".to_owned()),
            "{query}: {indexes:?}"
        );
    }
    // And the intarray operators return the right rows.
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM posts WHERE tag_ids @> '{5, 101}'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let expected = (1..=2000)
        .filter(|i| i % 50 == 5 && i % 7 + 100 == 101)
        .count();
    assert_eq!(count, expected as i64);
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn sort_orders_use_their_indexes(pool: PgPool) {
    seed_posts(&pool).await;
    let cases = [
        (
            "SELECT id FROM posts ORDER BY score DESC, id DESC LIMIT 20",
            "posts_score_idx",
        ),
        (
            "SELECT id FROM posts ORDER BY fav_count DESC, id DESC LIMIT 20",
            "posts_fav_count_idx",
        ),
        (
            "SELECT id FROM posts ORDER BY id DESC LIMIT 20",
            "posts_pkey",
        ),
    ];
    for (query, index) in cases {
        let indexes = plan_indexes(&pool, query).await;
        assert!(indexes.contains(&index.to_owned()), "{query}: {indexes:?}");
    }
}

#[sqlx::test(migrator = "crate::MIGRATOR")]
async fn post_constraints(pool: PgPool) {
    let bad_rating = sqlx::query("INSERT INTO posts (rating) VALUES ('x')")
        .execute(&pool)
        .await;
    assert!(bad_rating.is_err());

    let id: i64 = sqlx::query_scalar(
        "INSERT INTO posts (rating, tag_ids) VALUES ('e', '{1,2,3}') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let tag_count: i32 = sqlx::query_scalar("SELECT tag_count FROM posts WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tag_count, 3);

    let self_parent = sqlx::query("UPDATE posts SET parent_id = id WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await;
    assert!(self_parent.is_err());

    // Importers may choose IDs.
    sqlx::query("INSERT INTO posts (id, rating) VALUES (123456, 'g')")
        .execute(&pool)
        .await
        .unwrap();
}
