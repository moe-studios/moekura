//! The `tags.apply_relation` job, rewriting existing posts after a tag
//! alias or implication is approved, and `tags.mass_update`, adding and
//! removing tags on every post matching a search.

use moekura_core::config::SearchConfig;
use moekura_core::jobs::{ApplyTagRelation, MassUpdate};
use moekura_core::posts::PostStatus;
use moekura_core::search::Query;
use moekura_db::mass_updates;
use moekura_db::posts::Visibility;
use moekura_db::search::{PageRef, Plan, SearchError};
use moekura_db::tag_relations::{self, Kind, Status};
use moekura_db::tags::{self, WantedTag};
use sqlx::PgPool;

use crate::{JobError, Registry};

/// Posts rewritten per transaction; small enough that each batch holds
/// its row locks only briefly.
const BATCH: i64 = 500;

#[derive(Clone)]
pub struct TagJobs {
    pub db: PgPool,
}

impl TagJobs {
    pub fn register(self, registry: &mut Registry) {
        let jobs = self.clone();
        registry.register(move |job: ApplyTagRelation| {
            let jobs = jobs.clone();
            async move { jobs.apply(job.relation_id).await }
        });
        registry.register(move |job: MassUpdate| {
            let jobs = self.clone();
            async move {
                let result = jobs.mass_update(job.id).await;
                if let Err(JobError::Permanent(error)) = &result {
                    mass_updates::finish(&jobs.db, job.id, Some(error)).await?;
                }
                result
            }
        });
    }

    /// Posts tagged with the antecedent get the consequent and every tag it
    /// implies; for an alias, the antecedent is removed. Safe to repeat.
    pub async fn apply(&self, relation_id: i32) -> Result<(), JobError> {
        let Some(relation) = tag_relations::by_id(&self.db, relation_id).await? else {
            return Ok(());
        };
        // Removed again before the job ran.
        if relation.status != Status::Active {
            return Ok(());
        }
        // No tag means no posts to rewrite.
        let Some(antecedent) = tags::by_name(&self.db, &relation.antecedent).await? else {
            return Ok(());
        };

        let mut names = vec![relation.consequent.clone()];
        names.extend(tag_relations::implied_by(&self.db, &[&relation.consequent]).await?);
        // A new alias target takes the old tag's category.
        let category = (relation.kind == Kind::Alias && antecedent.category_id != 0)
            .then_some(antecedent.category_id);
        let wanted: Vec<WantedTag<'_>> = names
            .iter()
            .enumerate()
            .map(|(i, name)| WantedTag {
                name,
                category_id: if i == 0 { category } else { None },
            })
            .collect();
        let mut conn = self.db.acquire().await?;
        let mut add: Vec<i32> = tags::ensure(&mut conn, &wanted, false)
            .await?
            .iter()
            .map(|t| t.id)
            .collect();
        drop(conn);
        add.sort_unstable();

        let mut changed = 0;
        loop {
            let n = tag_relations::apply_batch(
                &self.db,
                Some(relation_id),
                relation.kind,
                antecedent.id,
                &add,
                BATCH,
            )
            .await?;
            if n == 0 {
                break;
            }
            changed += n;
        }
        tracing::info!(
            relation_id,
            kind = %relation.kind,
            antecedent = relation.antecedent,
            consequent = relation.consequent,
            changed,
            "tag relation applied"
        );
        Ok(())
    }
}

impl TagJobs {
    /// Adds and removes a mass edit's tags on every post its search
    /// matches, newest first, recording progress. Safe to repeat.
    pub async fn mass_update(&self, id: i64) -> Result<(), JobError> {
        let Some(update) = mass_updates::by_id(&self.db, id).await? else {
            return Ok(());
        };
        if update.status == "done" {
            return Ok(());
        }
        mass_updates::start(&self.db, id).await?;
        let mut query = Query::parse(&update.query).map_err(JobError::permanent)?;
        // Newest first by id, so every post is reached with keyset pages.
        query.order = None;
        query.ordfav = None;
        query.ordpool = None;
        query.ordfavgroup = None;
        query.limit = None;
        let config = SearchConfig {
            per_page: BATCH as u32,
            max_per_page: BATCH as u32,
            ..SearchConfig::default()
        };
        // Everything staff could see; deleted posts only with status:.
        let visibility = Visibility {
            statuses: vec![
                PostStatus::Active,
                PostStatus::Flagged,
                PostStatus::Pending,
                PostStatus::Deleted,
            ],
            viewer: None,
        };
        let plan = Plan::resolve(&self.db, &query, &visibility, &config)
            .await
            .map_err(search_error)?;

        let mut conn = self.db.acquire().await?;
        let wanted: Vec<WantedTag<'_>> = update
            .add_tags
            .iter()
            .map(|name| WantedTag {
                name,
                category_id: None,
            })
            .collect();
        // With what the added tags imply, as when tagging a post.
        let mut add: Vec<i32> = tags::for_post(&mut conn, &wanted, false)
            .await?
            .iter()
            .map(|t| t.id)
            .collect();
        drop(conn);
        add.sort_unstable();
        let mut remove = Vec::new();
        for name in &update.remove_tags {
            if let Some(tag) = tags::by_name(&self.db, name).await? {
                remove.push(tag.id);
            }
        }
        remove.sort_unstable();

        let (mut seen, mut changed) = (0i32, 0i32);
        let mut page = PageRef::Number(1);
        loop {
            let ids = plan.ids(&self.db, page).await.map_err(search_error)?;
            let Some(&last) = ids.last() else { break };
            let n = mass_updates::retag(&self.db, &ids, &add, &remove, update.creator_id).await?;
            seen += ids.len() as i32;
            changed += n as i32;
            mass_updates::progress(&self.db, id, seen, changed).await?;
            page = PageRef::Before(last);
        }
        mass_updates::finish(&self.db, id, None).await?;
        tracing::info!(
            id,
            query = update.query,
            seen,
            changed,
            "mass tag edit done"
        );
        Ok(())
    }
}

fn search_error(error: SearchError) -> JobError {
    match error {
        SearchError::Invalid(message) => JobError::permanent(message),
        SearchError::Db(error) => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use moekura_db::tag_relations::NewRequest;

    use super::*;

    async fn post(pool: &PgPool, names: &[&str]) -> i64 {
        let mut conn = pool.acquire().await.unwrap();
        let wanted: Vec<WantedTag<'_>> = names
            .iter()
            .map(|name| WantedTag {
                name,
                category_id: None,
            })
            .collect();
        let mut ids: Vec<i32> = tags::ensure(&mut conn, &wanted, false)
            .await
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();
        ids.sort_unstable();
        sqlx::query_scalar("INSERT INTO posts (rating, tag_ids) VALUES ('g', $1) RETURNING id")
            .bind(ids)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn tag_names(pool: &PgPool, post: i64) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT t.name FROM posts p JOIN tags t ON t.id = ANY(p.tag_ids)
             WHERE p.id = $1 ORDER BY t.name",
        )
        .bind(post)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn approved(pool: &PgPool, kind: Kind, antecedent: &str, consequent: &str) -> i32 {
        let admin: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'admin', id FROM roles WHERE system_key = 'admin'
             ON CONFLICT DO NOTHING RETURNING id",
        )
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap_or(1);
        let id = tag_relations::request(
            pool,
            NewRequest {
                kind,
                antecedent,
                consequent,
                reason: "",
                creator_id: None,
            },
        )
        .await
        .unwrap();
        tag_relations::approve(pool, id, admin).await.unwrap();
        id
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn mass_updates_retag_every_match(pool: PgPool) {
        let jobs = TagJobs { db: pool.clone() };
        let mut matching = Vec::new();
        for _ in 0..(BATCH + 3) {
            matching.push(post(&pool, &["cat_ears", "solo"]).await);
        }
        let other = post(&pool, &["dog"]).await;
        let already = post(&pool, &["cat_ears", "animal_ears"]).await;
        let admin: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'admin', id FROM roles WHERE system_key = 'admin' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let id = mass_updates::create(
            &pool,
            Some(admin),
            "cat_ears",
            &["animal_ears".to_owned()],
            &["cat_ears".to_owned()],
        )
        .await
        .unwrap();
        jobs.mass_update(id).await.unwrap();

        assert_eq!(tag_names(&pool, matching[0]).await, ["animal_ears", "solo"]);
        assert_eq!(
            tag_names(&pool, *matching.last().unwrap()).await,
            ["animal_ears", "solo"]
        );
        assert_eq!(tag_names(&pool, other).await, ["dog"]);
        assert_eq!(tag_names(&pool, already).await, ["animal_ears"]);
        let done = mass_updates::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (done.status.as_str(), done.seen, done.changed),
            ("done", BATCH as i32 + 4, BATCH as i32 + 4)
        );
        let latest = moekura_db::post_versions::list(&pool, matching[0])
            .await
            .unwrap()
            .remove(0);
        assert_eq!(latest.updater_id, Some(admin));
        // Again: nothing left to change.
        jobs.mass_update(id).await.unwrap();

        let bad = mass_updates::create(&pool, None, "~", &[], &[])
            .await
            .unwrap();
        assert!(matches!(
            jobs.mass_update(bad).await,
            Err(JobError::Permanent(_))
        ));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn aliases_and_implications_rewrite_posts(pool: PgPool) {
        let jobs = TagJobs { db: pool.clone() };
        let first = post(&pool, &["kitty", "cute"]).await;
        let second = post(&pool, &["kitty"]).await;
        let untouched = post(&pool, &["dog"]).await;
        sqlx::query("UPDATE tags SET category_id = 4 WHERE name = 'kitty'")
            .execute(&pool)
            .await
            .unwrap();

        let implication = approved(&pool, Kind::Implication, "cat", "animal").await;
        jobs.apply(implication).await.unwrap();
        let alias = approved(&pool, Kind::Alias, "kitty", "cat").await;
        jobs.apply(alias).await.unwrap();

        assert_eq!(tag_names(&pool, first).await, ["animal", "cat", "cute"]);
        // The rewrite shows in the post's history, credited to the alias.
        let latest = moekura_db::post_versions::list(&pool, first)
            .await
            .unwrap()
            .remove(0);
        assert_eq!(
            (
                latest.relation_kind.as_deref(),
                latest.relation_antecedent.as_deref()
            ),
            (Some("alias"), Some("kitty"))
        );
        assert_eq!(tag_names(&pool, second).await, ["animal", "cat"]);
        assert_eq!(tag_names(&pool, untouched).await, ["dog"]);
        let cat = tags::by_name(&pool, "cat").await.unwrap().unwrap();
        assert_eq!((cat.category_id, cat.post_count), (4, 2));
        assert_eq!(
            tags::by_name(&pool, "kitty")
                .await
                .unwrap()
                .unwrap()
                .post_count,
            0
        );

        // Later posts get the same treatment when tagged.
        let mut conn = pool.acquire().await.unwrap();
        let mut names: Vec<String> = tags::for_post(
            &mut conn,
            &[WantedTag {
                name: "kitty",
                category_id: None,
            }],
            false,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
        names.sort();
        assert_eq!(names, ["animal", "cat"]);

        // A removed relation's job does nothing.
        tag_relations::remove(&pool, implication, 1).await.unwrap();
        let third = post(&pool, &["cat"]).await;
        jobs.apply(implication).await.unwrap();
        assert_eq!(tag_names(&pool, third).await, ["cat"]);
    }
}
