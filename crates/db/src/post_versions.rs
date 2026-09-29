//! Post history. Versions are recorded by triggers on `posts` (migrations
//! 0012 and 0046) whenever tags, rating, source, description, parent or
//! locks change;
//! [`attribute`] tells them who is making the change.

use moekura_core::posts::{PostLock, Rating};
use sqlx::{PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;

use crate::posts::{PostEdit, Visibility};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Version {
    pub id: i64,
    pub version: i32,
    pub updater_id: Option<i64>,
    pub updater_name: Option<String>,
    /// For changes made by a tag alias or implication.
    pub relation_kind: Option<String>,
    pub relation_antecedent: Option<String>,
    pub relation_consequent: Option<String>,
    pub tag_ids: Vec<i32>,
    pub added_tag_ids: Vec<i32>,
    pub removed_tag_ids: Vec<i32>,
    pub rating: String,
    pub source: String,
    pub description: String,
    pub parent_id: Option<i64>,
    /// The post's locks (`moekura_core::posts::PostLock` names).
    pub locks: Vec<String>,
    pub created_at: OffsetDateTime,
}

/// `SELECT <version columns> …` followed by `$rest`.
macro_rules! select_versions {
    ($rest:literal) => {
        concat!(
            "SELECT v.id, v.version, v.updater_id, u.name::text AS updater_name, r.kind AS relation_kind,
                    r.antecedent_name::text AS relation_antecedent,
                    r.consequent_name::text AS relation_consequent,
                    v.tag_ids, v.added_tag_ids, v.removed_tag_ids, v.rating, v.source,
                    v.description, v.parent_id, v.locks, v.created_at
             FROM post_versions v
             LEFT JOIN users u ON u.id = v.updater_id
             LEFT JOIN tag_relations r ON r.id = v.relation_id ",
            $rest
        )
    };
}

/// Attributes post changes in the current transaction to a user, or to a
/// tag relation. Call it inside the transaction making the change.
pub async fn attribute(
    conn: &mut PgConnection,
    updater_id: Option<i64>,
    relation_id: Option<i32>,
) -> sqlx::Result<()> {
    let text = |v: Option<String>| v.unwrap_or_default();
    sqlx::query(
        "SELECT set_config('moekura.updater_id', $1, true), set_config('moekura.relation_id', $2, true)",
    )
    .bind(text(updater_id.map(|id| id.to_string())))
    .bind(text(relation_id.map(|id| id.to_string())))
    .execute(conn)
    .await?;
    Ok(())
}

/// A post's versions, newest first (at most the latest 500).
pub async fn list(db: impl PgExecutor<'_>, post_id: i64) -> sqlx::Result<Vec<Version>> {
    sqlx::query_as(select_versions!(
        "WHERE v.post_id = $1 ORDER BY v.version DESC LIMIT 500"
    ))
    .bind(post_id)
    .fetch_all(db)
    .await
}

pub async fn get(
    db: impl PgExecutor<'_>,
    post_id: i64,
    version: i32,
) -> sqlx::Result<Option<Version>> {
    sqlx::query_as(select_versions!("WHERE v.post_id = $1 AND v.version = $2"))
        .bind(post_id)
        .bind(version)
        .fetch_optional(db)
        .await
}

/// Which versions [`search`] finds; `None` matches any.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub updater_id: Option<i64>,
    pub post_id: Option<i64>,
    /// Versions that added this tag.
    pub added_tag: Option<i32>,
    /// Versions that removed this tag.
    pub removed_tag: Option<i32>,
    /// Made at or after this time.
    pub since: Option<OffsetDateTime>,
    /// Made before this time.
    pub until: Option<OffsetDateTime>,
    /// Older than this version id (keyset pagination).
    pub before: Option<i64>,
}

/// A version with what the one before it had, for showing what changed.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Change {
    pub post_id: i64,
    #[sqlx(flatten)]
    pub version: Version,
    pub previous_rating: Option<String>,
    pub previous_source: Option<String>,
    pub previous_description: Option<String>,
    pub previous_parent_id: Option<i64>,
    pub previous_locks: Option<Vec<String>>,
    /// Whether there is a version before it (not so for uploads).
    pub has_previous: bool,
}

/// Versions of posts `visibility` allows, across the site, newest first.
pub async fn search(
    db: impl PgExecutor<'_>,
    filter: &Filter,
    visibility: &Visibility,
    limit: i64,
) -> sqlx::Result<Vec<Change>> {
    let statuses: Vec<&str> = visibility.statuses.iter().map(|s| s.as_str()).collect();
    sqlx::query_as(
        "SELECT v.post_id, v.id, v.version, v.updater_id, u.name::text AS updater_name,
                r.kind AS relation_kind, r.antecedent_name::text AS relation_antecedent,
                r.consequent_name::text AS relation_consequent,
                v.tag_ids, v.added_tag_ids, v.removed_tag_ids, v.rating, v.source,
                v.description, v.parent_id, v.locks, v.created_at,
                pv.rating AS previous_rating, pv.source AS previous_source,
                pv.description AS previous_description, pv.parent_id AS previous_parent_id,
                pv.locks AS previous_locks, pv.id IS NOT NULL AS has_previous
         FROM post_versions v
         JOIN posts p ON p.id = v.post_id
         LEFT JOIN post_versions pv ON pv.post_id = v.post_id AND pv.version = v.version - 1
         LEFT JOIN users u ON u.id = v.updater_id
         LEFT JOIN tag_relations r ON r.id = v.relation_id
         WHERE (p.status = ANY($1) OR (p.status = 'pending' AND p.uploader_id = $2))
           AND p.rating = ANY($11)
           AND ($3::bigint IS NULL OR v.updater_id = $3)
           AND ($4::bigint IS NULL OR v.post_id = $4)
           AND ($5::int IS NULL OR v.added_tag_ids @> ARRAY[$5::int])
           AND ($6::int IS NULL OR v.removed_tag_ids @> ARRAY[$6::int])
           AND ($7::timestamptz IS NULL OR v.created_at >= $7)
           AND ($8::timestamptz IS NULL OR v.created_at < $8)
           AND ($9::bigint IS NULL OR v.id < $9)
         ORDER BY v.id DESC LIMIT $10",
    )
    .bind(statuses)
    .bind(visibility.viewer)
    .bind(filter.updater_id)
    .bind(filter.post_id)
    .bind(filter.added_tag)
    .bind(filter.removed_tag)
    .bind(filter.since)
    .bind(filter.until)
    .bind(filter.before)
    .bind(limit)
    .bind(visibility.rating_codes())
    .fetch_all(db)
    .await
}

/// A post's fields as a version leaves them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fields {
    tag_ids: Vec<i32>,
    rating: String,
    source: String,
    description: String,
    parent_id: Option<i64>,
}

/// One of a user's versions and the version before it.
#[derive(sqlx::FromRow)]
struct Undoable {
    added_tag_ids: Vec<i32>,
    removed_tag_ids: Vec<i32>,
    rating: String,
    source: String,
    description: String,
    parent_id: Option<i64>,
    previous_rating: String,
    previous_source: String,
    previous_description: String,
    previous_parent_id: Option<i64>,
}

/// `fields` without what `change` did, where nobody changed it since:
/// its added tags come off and its removed tags go back, and a rating,
/// source, description or parent it set is put back if the post still
/// has it. `locks` are left alone.
fn undo(fields: &mut Fields, change: &Undoable, locks: &[PostLock]) {
    if !locks.contains(&PostLock::Tags) {
        fields
            .tag_ids
            .retain(|id| !change.added_tag_ids.contains(id));
        fields.tag_ids.extend(&change.removed_tag_ids);
        fields.tag_ids.sort_unstable();
        fields.tag_ids.dedup();
    }
    if !locks.contains(&PostLock::Rating)
        && change.rating != change.previous_rating
        && fields.rating == change.rating
    {
        fields.rating.clone_from(&change.previous_rating);
    }
    if change.source != change.previous_source && fields.source == change.source {
        fields.source.clone_from(&change.previous_source);
    }
    if change.description != change.previous_description && fields.description == change.description
    {
        fields.description.clone_from(&change.previous_description);
    }
    if change.parent_id != change.previous_parent_id && fields.parent_id == change.parent_id {
        fields.parent_id = change.previous_parent_id;
    }
}

/// Undoes the post edits `user_id` made from `since` until `until`
/// (either open-ended), newest first, as `actor_id` in each post's
/// history. Uploads aren't edits and stay. Returns how many posts
/// changed.
pub async fn undo_user(
    db: &PgPool,
    user_id: i64,
    since: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
    actor_id: Option<i64>,
) -> sqlx::Result<u64> {
    let posts: Vec<i64> = sqlx::query_scalar(
        "SELECT DISTINCT post_id FROM post_versions
         WHERE updater_id = $1 AND version > 1
           AND ($2::timestamptz IS NULL OR created_at >= $2)
           AND ($3::timestamptz IS NULL OR created_at < $3)
         ORDER BY post_id",
    )
    .bind(user_id)
    .bind(since)
    .bind(until)
    .fetch_all(db)
    .await?;
    let mut changed = 0;
    for post_id in posts {
        let mut tx = db.begin().await?;
        let Some(post) = crate::posts::lock(&mut *tx, post_id).await? else {
            continue;
        };
        let changes: Vec<Undoable> = sqlx::query_as(
            "SELECT v.added_tag_ids, v.removed_tag_ids, v.rating, v.source, v.description,
                    v.parent_id, pv.rating AS previous_rating, pv.source AS previous_source,
                    pv.description AS previous_description, pv.parent_id AS previous_parent_id
             FROM post_versions v
             JOIN post_versions pv ON pv.post_id = v.post_id AND pv.version = v.version - 1
             WHERE v.post_id = $1 AND v.updater_id = $2
               AND ($3::timestamptz IS NULL OR v.created_at >= $3)
               AND ($4::timestamptz IS NULL OR v.created_at < $4)
             ORDER BY v.version DESC",
        )
        .bind(post_id)
        .bind(user_id)
        .bind(since)
        .bind(until)
        .fetch_all(&mut *tx)
        .await?;
        let before = Fields {
            tag_ids: post.tag_ids.clone(),
            rating: post.rating.code().to_owned(),
            source: post.source.clone(),
            description: post.description.clone(),
            parent_id: post.parent_id,
        };
        let mut after = before.clone();
        for change in &changes {
            undo(&mut after, change, &post.locks);
        }
        // A parent removed since, or now below the post, is dropped.
        if let Some(parent) = after.parent_id
            && after.parent_id != before.parent_id
            && (crate::posts::by_id(&mut *tx, parent).await?.is_none()
                || crate::posts::has_ancestor(&mut *tx, parent, post_id).await?)
        {
            after.parent_id = before.parent_id;
        }
        if after == before {
            continue;
        }
        let rating: Rating = after.rating.parse().unwrap_or(post.rating);
        attribute(&mut tx, actor_id, None).await?;
        crate::posts::update(
            &mut *tx,
            post_id,
            PostEdit {
                rating,
                source: &after.source,
                description: &after.description,
                parent_id: after.parent_id,
                tag_ids: &after.tag_ids,
            },
        )
        .await?;
        tx.commit().await?;
        changed += 1;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn changes_are_recorded_and_attributed(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        let ids = crate::tags::tests::create(&pool, &["a", "b", "c"]).await;
        let post: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, uploader_id, tag_ids) VALUES ('g', $1, $2) RETURNING id",
        )
        .bind(alice)
        .bind(&ids[..2])
        .fetch_one(&pool)
        .await
        .unwrap();

        let mut tx = pool.begin().await.unwrap();
        attribute(&mut tx, Some(bob), None).await.unwrap();
        sqlx::query("UPDATE posts SET tag_ids = $2, rating = 'e' WHERE id = $1")
            .bind(post)
            .bind(&ids[1..])
            .execute(&mut *tx)
            .await
            .unwrap();
        // Changes to other columns aren't versions.
        sqlx::query("UPDATE posts SET score = 3 WHERE id = $1")
            .bind(post)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        // Attribution ends with the transaction.
        sqlx::query("UPDATE posts SET source = 'x' WHERE id = $1")
            .bind(post)
            .execute(&pool)
            .await
            .unwrap();

        let versions = list(&pool, post).await.unwrap();
        let summary: Vec<_> = versions
            .iter()
            .map(|v| {
                (
                    v.version,
                    v.updater_name.as_deref(),
                    v.added_tag_ids.clone(),
                    v.removed_tag_ids.clone(),
                    v.rating.as_str(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (3, None, vec![], vec![], "e"),
                (2, Some("bob"), vec![ids[2]], vec![ids[0]], "e"),
                (1, Some("alice"), ids[..2].to_vec(), vec![], "g"),
            ]
        );
        assert_eq!(get(&pool, post, 3).await.unwrap().unwrap().source, "x");
        assert!(get(&pool, post, 4).await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn searching_and_undoing_a_users_edits(pool: PgPool) {
        use moekura_core::posts::PostStatus;

        let alice = user(&pool, "alice").await;
        let vandal = user(&pool, "vandal").await;
        let ids = crate::tags::tests::create(&pool, &["a", "b", "c", "d"]).await;
        let post: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, uploader_id, tag_ids, source) VALUES ('g', $1, $2, 'orig') RETURNING id",
        )
        .bind(alice)
        .bind(&ids[..2])
        .fetch_one(&pool)
        .await
        .unwrap();
        let edit = |by: i64, sql: &'static str, remove: i32, add: i32| {
            let pool = pool.clone();
            async move {
                let mut tx = pool.begin().await.unwrap();
                attribute(&mut tx, Some(by), None).await.unwrap();
                sqlx::query(sql)
                    .bind(post)
                    .bind(remove)
                    .bind(add)
                    .execute(&mut *tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
            }
        };
        // The vandal swaps a for c and makes it explicit; alice then adds d.
        edit(
            vandal,
            "UPDATE posts SET tag_ids = sort(tag_ids - $2::int | $3::int), rating = 'e',
                              source = 'spam' WHERE id = $1",
            ids[0],
            ids[2],
        )
        .await;
        edit(
            alice,
            "UPDATE posts SET tag_ids = sort(tag_ids - $2::int | $3::int) WHERE id = $1",
            ids[2],
            ids[3],
        )
        .await;

        let everyone = Visibility {
            statuses: vec![PostStatus::Active],
            viewer: None,
            ratings: Vec::new(),
        };
        let by_vandal = Filter {
            updater_id: Some(vandal),
            ..Filter::default()
        };
        let found = search(&pool, &by_vandal, &everyone, 10).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].previous_rating.as_deref(), Some("g"));
        assert!(found[0].has_previous);
        let removing_a = Filter {
            removed_tag: Some(ids[0]),
            ..Filter::default()
        };
        assert_eq!(
            search(&pool, &removing_a, &everyone, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        let adding_d = Filter {
            added_tag: Some(ids[3]),
            ..Filter::default()
        };
        assert_eq!(
            search(&pool, &adding_d, &everyone, 10).await.unwrap()[0]
                .version
                .updater_name
                .as_deref(),
            Some("alice")
        );
        let hidden = Visibility {
            statuses: vec![PostStatus::Deleted],
            viewer: None,
            ratings: Vec::new(),
        };
        assert!(
            search(&pool, &Filter::default(), &hidden, 10)
                .await
                .unwrap()
                .is_empty()
        );

        // Undoing keeps alice's later edit.
        assert_eq!(
            undo_user(&pool, vandal, None, None, Some(alice))
                .await
                .unwrap(),
            1
        );
        let (tag_ids, rating, source): (Vec<i32>, String, String) =
            sqlx::query_as("SELECT tag_ids, rating, source FROM posts WHERE id = $1")
                .bind(post)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(tag_ids, [ids[0], ids[1], ids[3]]);
        assert_eq!((rating.as_str(), source.as_str()), ("g", "orig"));
        let latest = &list(&pool, post).await.unwrap()[0];
        assert_eq!(latest.updater_name.as_deref(), Some("alice"));
        // Nothing left to undo; alice's upload isn't an edit.
        assert_eq!(undo_user(&pool, vandal, None, None, None).await.unwrap(), 0);
        let past = OffsetDateTime::now_utc() - time::Duration::days(1);
        assert_eq!(
            undo_user(&pool, alice, None, Some(past), None)
                .await
                .unwrap(),
            0
        );
    }
}
