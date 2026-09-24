//! Search result counts that hit the count limit ("10,000+"), reused for
//! `cache.count_ttl_secs`. Those are the costliest counts, since counting
//! reads up to the limit, and a few new posts don't change them. Smaller
//! counts stay exact and fresh; estimates are cheap anyway. Cached in
//! Valkey when it's the cache backend (shared by every web server), in
//! memory otherwise.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use moekura_db::search::{Count, Plan, SearchError};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::auth::CurrentUser;
use crate::shared::Valkey;

/// Most counts kept in memory.
const MEMORY_ENTRIES: usize = 10_000;

pub struct CountCache {
    ttl: Duration,
    valkey: Option<Valkey>,
    memory: Mutex<HashMap<String, (Instant, Count)>>,
}

fn encode(count: Count) -> String {
    match count {
        Count::Exact(n) => format!("e{n}"),
        Count::About(n) => format!("a{n}"),
        Count::AtLeast(n) => format!("l{n}"),
    }
}

fn decode(text: &str) -> Option<Count> {
    let (kind, n) = text.split_at_checked(1)?;
    let n = n.parse().ok()?;
    match kind {
        "e" => Some(Count::Exact(n)),
        "a" => Some(Count::About(n)),
        "l" => Some(Count::AtLeast(n)),
        _ => None,
    }
}

impl CountCache {
    pub fn new(ttl: Duration, valkey: Option<Valkey>) -> Self {
        Self {
            ttl,
            valkey,
            memory: Mutex::new(HashMap::new()),
        }
    }

    /// `plan`'s count, from the cache when fresh. Someone who just changed
    /// something gets a fresh count, so they see their change.
    pub async fn count(
        &self,
        plan: &Plan,
        db: &PgPool,
        current: &CurrentUser,
    ) -> Result<Count, SearchError> {
        if self.ttl.is_zero() {
            return plan.count(db).await;
        }
        let key = format!(
            "count:{}",
            hex::encode(Sha256::digest(plan.fingerprint().as_bytes()))
        );
        if !current.recent_write
            && let Some(count) = self.get(&key).await
        {
            return Ok(count);
        }
        let count = plan.count(db).await?;
        if matches!(count, Count::AtLeast(_)) {
            self.put(&key, count).await;
        }
        Ok(count)
    }

    async fn get(&self, key: &str) -> Option<Count> {
        if let Some(valkey) = &self.valkey {
            match valkey.get(key).await {
                Ok(value) => return value.as_deref().and_then(decode),
                Err(error) => tracing::debug!(%error, "count cache unavailable"),
            }
            return None;
        }
        let memory = self.memory.lock().expect("count cache lock");
        memory
            .get(key)
            .filter(|(at, _)| at.elapsed() < self.ttl)
            .map(|(_, count)| *count)
    }

    async fn put(&self, key: &str, count: Count) {
        if let Some(valkey) = &self.valkey {
            if let Err(error) = valkey.set(key, &encode(count), self.ttl).await {
                tracing::debug!(%error, "count cache unavailable");
            }
            return;
        }
        let mut memory = self.memory.lock().expect("count cache lock");
        if memory.len() >= MEMORY_ENTRIES {
            let ttl = self.ttl;
            memory.retain(|_, (at, _)| at.elapsed() < ttl);
            if memory.len() >= MEMORY_ENTRIES {
                memory.clear();
            }
        }
        memory.insert(key.to_owned(), (Instant::now(), count));
    }
}

#[cfg(test)]
mod tests {
    use moekura_core::config::SearchConfig;
    use moekura_core::posts::PostStatus;
    use moekura_core::search::Query;
    use moekura_db::posts::Visibility;

    use super::*;
    use crate::test_support::test_state;

    async fn check_reuse(pool: &PgPool, cache: CountCache) {
        for _ in 0..3 {
            sqlx::query("INSERT INTO posts (rating) VALUES ('g')")
                .execute(pool)
                .await
                .unwrap();
        }
        // A tiny count limit makes every count hit it.
        let config = SearchConfig {
            count_limit: 2,
            ..SearchConfig::default()
        };
        let visible = Visibility {
            statuses: vec![PostStatus::Active],
            viewer: None,
        };
        let search = async |input: &str| {
            let query = Query::parse(input).unwrap();
            Plan::resolve(pool, &query, &visible, &config)
                .await
                .unwrap()
        };
        let state = test_state(pool).await;
        let visitor = crate::auth::tests_support_visitor(&state);
        // A filter keeps the count from being estimated from the table size.
        let plan = search("score:0").await;
        assert_eq!(
            cache.count(&plan, pool, &visitor).await.unwrap(),
            Count::AtLeast(2)
        );
        sqlx::query("DELETE FROM posts")
            .execute(pool)
            .await
            .unwrap();
        // Reused while fresh...
        assert_eq!(
            cache.count(&plan, pool, &visitor).await.unwrap(),
            Count::AtLeast(2)
        );
        // ...except for someone who just changed something.
        let mut writer = visitor.clone();
        writer.recent_write = true;
        assert_eq!(
            cache.count(&plan, pool, &writer).await.unwrap(),
            Count::Exact(0)
        );
        // Small exact counts aren't cached.
        let other = search("score:5").await;
        assert_eq!(
            cache.count(&other, pool, &visitor).await.unwrap(),
            Count::Exact(0)
        );
        for _ in 0..3 {
            sqlx::query("INSERT INTO posts (rating, score) VALUES ('g', 5)")
                .execute(pool)
                .await
                .unwrap();
        }
        assert_eq!(
            cache.count(&other, pool, &visitor).await.unwrap(),
            Count::AtLeast(2)
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn reuses_counts_at_the_limit_in_memory(pool: PgPool) {
        check_reuse(&pool, CountCache::new(Duration::from_secs(60), None)).await;
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn reuses_counts_at_the_limit_in_valkey(pool: PgPool) {
        let Some(valkey) = crate::shared::tests::valkey().await else {
            eprintln!("skipping: TEST_VALKEY_URL not set");
            return;
        };
        check_reuse(
            &pool,
            CountCache::new(Duration::from_secs(60), Some(valkey)),
        )
        .await;
    }

    #[test]
    fn encodes_counts() {
        for count in [
            Count::Exact(0),
            Count::About(12_345),
            Count::AtLeast(10_000),
        ] {
            assert_eq!(decode(&encode(count)), Some(count));
        }
        assert_eq!(decode(""), None);
        assert_eq!(decode("x1"), None);
    }
}
