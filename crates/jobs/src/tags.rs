//! The `tags.apply_relation` job: rewriting existing posts after a tag
//! alias or implication is approved.

use moekura_core::jobs::ApplyTagRelation;
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
        registry.register(move |job: ApplyTagRelation| {
            let jobs = self.clone();
            async move { jobs.apply(job.relation_id).await }
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
