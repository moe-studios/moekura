//! Tag aliases and implications: requests, approval, and applying them.
//!
//! The rules that keep them consistent (no alias chains, no aliasing a tag
//! that has implications, no implication cycles) are checked on request,
//! for early feedback, and again on approval under a lock, since other
//! approvals may have happened in between.

use std::fmt;
use std::str::FromStr;

use moekura_core::jobs::ApplyTagRelation;
use sqlx::{PgConnection, PgExecutor, PgPool};
use time::OffsetDateTime;

/// Serialises approvals, so two concurrent ones can't form a cycle or a
/// chain that each alone would not.
const APPROVAL_LOCK: i64 = 0x7577_7575_7461_6773;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Alias,
    Implication,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Alias => "alias",
            Kind::Implication => "implication",
        }
    }
}

impl FromStr for Kind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "alias" => Ok(Kind::Alias),
            "implication" => Ok(Kind::Implication),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    Pending,
    Active,
    Rejected,
    Deleted,
}

impl Status {
    pub const ALL: [Status; 4] = [
        Status::Pending,
        Status::Active,
        Status::Rejected,
        Status::Deleted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Active => "active",
            Status::Rejected => "rejected",
            Status::Deleted => "deleted",
        }
    }
}

impl FromStr for Status {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|v| v.as_str() == s).ok_or(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub id: i32,
    pub kind: Kind,
    pub antecedent: String,
    pub consequent: String,
    pub status: Status,
    pub reason: String,
    pub creator_id: Option<i64>,
    pub creator_name: Option<String>,
    pub approver_name: Option<String>,
    /// Votes for, less votes against.
    pub score: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(sqlx::FromRow)]
struct RelationRow {
    id: i32,
    kind: String,
    antecedent_name: String,
    consequent_name: String,
    status: String,
    reason: String,
    creator_id: Option<i64>,
    creator_name: Option<String>,
    approver_name: Option<String>,
    score: i32,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

impl TryFrom<RelationRow> for Relation {
    type Error = sqlx::Error;

    fn try_from(row: RelationRow) -> Result<Self, Self::Error> {
        let bad = |what: &str, value: &str| {
            sqlx::Error::Decode(format!("unknown tag relation {what} `{value}`").into())
        };
        Ok(Relation {
            id: row.id,
            kind: row.kind.parse().map_err(|()| bad("kind", &row.kind))?,
            antecedent: row.antecedent_name,
            consequent: row.consequent_name,
            status: row
                .status
                .parse()
                .map_err(|()| bad("status", &row.status))?,
            reason: row.reason,
            creator_id: row.creator_id,
            creator_name: row.creator_name,
            approver_name: row.approver_name,
            score: row.score,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

/// `SELECT <relation columns> FROM tag_relations r` with user names,
/// followed by `$rest`.
macro_rules! select_relations {
    ($rest:literal) => {
        concat!(
            "SELECT r.id, r.kind, r.antecedent_name, r.consequent_name, r.status, r.reason,
                    r.creator_id, c.name::text AS creator_name, a.name::text AS approver_name,
                    r.score, r.created_at, r.updated_at
             FROM tag_relations r
             LEFT JOIN users c ON c.id = r.creator_id
             LEFT JOIN users a ON a.id = r.approver_id ",
            $rest
        )
    };
}

/// Why a request can't be made or approved. The messages are shown to
/// users.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    #[error("That request already exists.")]
    Duplicate,
    #[error("`{0}` is already aliased to `{1}`.")]
    AlreadyAliased(String, String),
    #[error("`{0}` is aliased to `{1}`; use `{1}` instead.")]
    AliasedAway(String, String),
    #[error("`{0}` has implications; remove them before aliasing it.")]
    HasImplications(String),
    #[error("`{1}` already implies `{0}`, so this would create a loop.")]
    Cycle(String, String),
    #[error("`{0}` already implies `{1}`.")]
    Redundant(String, String),
    #[error("Only pending requests can be approved or rejected.")]
    NotPending,
}

#[derive(Debug, thiserror::Error)]
pub enum RelationError {
    #[error(transparent)]
    Rule(#[from] RuleError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i32) -> sqlx::Result<Option<Relation>> {
    let row: Option<RelationRow> = sqlx::query_as(select_relations!("WHERE r.id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await?;
    row.map(Relation::try_from).transpose()
}

/// Newest first. `name` matches either side by prefix when not empty.
pub async fn list(
    db: impl PgExecutor<'_>,
    kind: Kind,
    status: Option<Status>,
    name: &str,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Relation>> {
    let prefix = crate::tags::like_pattern(name);
    let rows: Vec<RelationRow> = sqlx::query_as(select_relations!(
        "WHERE r.kind = $1 AND ($2::text IS NULL OR r.status = $2)
           AND ($3 = '%' OR r.antecedent_name LIKE $3 OR r.consequent_name LIKE $3)
         ORDER BY r.id DESC OFFSET $4 LIMIT $5"
    ))
    .bind(kind.as_str())
    .bind(status.map(Status::as_str))
    .bind(prefix)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await?;
    rows.into_iter().map(Relation::try_from).collect()
}

/// Relations of `kind` by exact names: `antecedent`, `consequent`, or
/// `either` side, newest first.
#[allow(clippy::too_many_arguments)]
pub async fn find(
    db: impl PgExecutor<'_>,
    kind: Kind,
    status: Option<Status>,
    antecedent: Option<&str>,
    consequent: Option<&str>,
    either: Option<&str>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<Relation>> {
    let rows: Vec<RelationRow> = sqlx::query_as(select_relations!(
        "WHERE r.kind = $1 AND ($2::text IS NULL OR r.status = $2)
           AND ($3::text IS NULL OR r.antecedent_name = $3)
           AND ($4::text IS NULL OR r.consequent_name = $4)
           AND ($5::text IS NULL OR r.antecedent_name = $5 OR r.consequent_name = $5)
         ORDER BY r.id DESC OFFSET $6 LIMIT $7"
    ))
    .bind(kind.as_str())
    .bind(status.map(Status::as_str))
    .bind(antecedent)
    .bind(consequent)
    .bind(either)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await?;
    rows.into_iter().map(Relation::try_from).collect()
}

/// The active aliases among `names`, as (antecedent, consequent).
pub async fn aliases_of(
    db: impl PgExecutor<'_>,
    names: &[&str],
) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as(
        "SELECT antecedent_name, consequent_name FROM tag_relations
         WHERE kind = 'alias' AND status = 'active' AND antecedent_name = ANY($1)",
    )
    .bind(names)
    .fetch_all(db)
    .await
}

/// Every tag implied by `names`, directly or through other implications,
/// excluding `names` themselves.
pub async fn implied_by(db: impl PgExecutor<'_>, names: &[&str]) -> sqlx::Result<Vec<String>> {
    // UNION (not UNION ALL) stops at tags already seen, so even a cycle
    // that slipped in can't loop forever.
    sqlx::query_scalar(
        "WITH RECURSIVE implied (name) AS (
             SELECT consequent_name FROM tag_relations
             WHERE kind = 'implication' AND status = 'active' AND antecedent_name = ANY($1)
             UNION
             SELECT r.consequent_name FROM tag_relations r
             JOIN implied ON r.antecedent_name = implied.name
             WHERE r.kind = 'implication' AND r.status = 'active'
         )
         SELECT name FROM implied WHERE NOT name = ANY($1) ORDER BY name",
    )
    .bind(names)
    .fetch_all(db)
    .await
}

async fn alias_target(conn: &mut PgConnection, name: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "SELECT consequent_name FROM tag_relations
         WHERE kind = 'alias' AND status = 'active' AND antecedent_name = $1",
    )
    .bind(name)
    .fetch_optional(conn)
    .await
}

/// Checks the rules for `antecedent → consequent` against the active
/// relations, ignoring the relation `except` (the one being approved).
async fn check_rules(
    conn: &mut PgConnection,
    kind: Kind,
    antecedent: &str,
    consequent: &str,
    except: Option<i32>,
) -> Result<(), RelationError> {
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM tag_relations
             WHERE kind = $1 AND antecedent_name = $2 AND consequent_name = $3
               AND status IN ('pending', 'active') AND id IS DISTINCT FROM $4)",
    )
    .bind(kind.as_str())
    .bind(antecedent)
    .bind(consequent)
    .bind(except)
    .fetch_one(&mut *conn)
    .await?;
    if duplicate {
        return Err(RuleError::Duplicate.into());
    }
    if let Some(target) = alias_target(conn, antecedent).await? {
        return Err(match kind {
            Kind::Alias => RuleError::AlreadyAliased(antecedent.into(), target),
            Kind::Implication => RuleError::AliasedAway(antecedent.into(), target),
        }
        .into());
    }
    if let Some(target) = alias_target(conn, consequent).await? {
        return Err(RuleError::AliasedAway(consequent.into(), target).into());
    }
    match kind {
        Kind::Alias => {
            let has_implications: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                     SELECT 1 FROM tag_relations
                     WHERE kind = 'implication' AND status = 'active'
                       AND (antecedent_name = $1 OR consequent_name = $1))",
            )
            .bind(antecedent)
            .fetch_one(&mut *conn)
            .await?;
            if has_implications {
                return Err(RuleError::HasImplications(antecedent.into()).into());
            }
        }
        Kind::Implication => {
            if implied_by(&mut *conn, &[consequent])
                .await?
                .iter()
                .any(|n| n == antecedent)
            {
                return Err(RuleError::Cycle(antecedent.into(), consequent.into()).into());
            }
            if implied_by(&mut *conn, &[antecedent])
                .await?
                .iter()
                .any(|n| n == consequent)
            {
                return Err(RuleError::Redundant(antecedent.into(), consequent.into()).into());
            }
        }
    }
    Ok(())
}

pub struct NewRequest<'a> {
    pub kind: Kind,
    /// Normalised tag names.
    pub antecedent: &'a str,
    pub consequent: &'a str,
    pub reason: &'a str,
    pub creator_id: Option<i64>,
}

/// Records a pending request, after checking it could be approved as
/// things stand. Returns its id.
pub async fn request(db: &PgPool, new: NewRequest<'_>) -> Result<i32, RelationError> {
    let mut tx = db.begin().await?;
    check_rules(&mut tx, new.kind, new.antecedent, new.consequent, None).await?;
    let id = sqlx::query_scalar(
        "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, reason, creator_id)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(new.kind.as_str())
    .bind(new.antecedent)
    .bind(new.consequent)
    .bind(new.reason)
    .bind(new.creator_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        // A concurrent identical request.
        sqlx::Error::Database(db) if db.is_unique_violation() => RuleError::Duplicate.into(),
        _ => RelationError::Db(e),
    })?;
    tx.commit().await?;
    Ok(id)
}

/// Approves a pending request and queues the rewrite of existing posts.
/// Approving an alias `a → b` also repoints aliases `x → a` to `b`.
pub async fn approve(db: &PgPool, id: i32, approver_id: i64) -> Result<(), RelationError> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(APPROVAL_LOCK)
        .execute(&mut *tx)
        .await?;
    // Against a concurrent rejection or removal.
    sqlx::query("SELECT 1 FROM tag_relations WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let Some(relation) = by_id(&mut *tx, id).await? else {
        return Err(RuleError::NotPending.into());
    };
    if relation.status != Status::Pending {
        return Err(RuleError::NotPending.into());
    }
    check_rules(
        &mut tx,
        relation.kind,
        &relation.antecedent,
        &relation.consequent,
        Some(id),
    )
    .await?;
    if relation.kind == Kind::Alias {
        sqlx::query(
            "UPDATE tag_relations SET consequent_name = $2, updated_at = now()
             WHERE kind = 'alias' AND status = 'active' AND consequent_name = $1",
        )
        .bind(&relation.antecedent)
        .bind(&relation.consequent)
        .execute(&mut *tx)
        .await?;
    }
    set_status(&mut tx, id, Status::Active, approver_id).await?;
    crate::jobs::enqueue(&mut tx, &ApplyTagRelation { relation_id: id }).await?;
    tx.commit().await?;
    Ok(())
}

async fn set_status(conn: &mut PgConnection, id: i32, status: Status, by: i64) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE tag_relations SET status = $2, approver_id = $3, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status.as_str())
    .bind(by)
    .execute(conn)
    .await?;
    Ok(())
}

/// Rejects a pending request.
pub async fn reject(db: &PgPool, id: i32, by: i64) -> Result<(), RelationError> {
    let mut tx = db.begin().await?;
    let pending: Option<String> =
        sqlx::query_scalar("SELECT status FROM tag_relations WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if pending.as_deref() != Some("pending") {
        return Err(RuleError::NotPending.into());
    }
    set_status(&mut tx, id, Status::Rejected, by).await?;
    tx.commit().await?;
    Ok(())
}

/// Withdraws a pending request or retires an active relation. Posts
/// already rewritten stay as they are. Returns false if there was nothing
/// to remove.
pub async fn remove(db: &PgPool, id: i32, by: i64) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE tag_relations SET status = 'deleted', approver_id = $2, updated_at = now()
         WHERE id = $1 AND status IN ('pending', 'active')",
    )
    .bind(id)
    .bind(by)
    .execute(db)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Rewrites up to `limit` posts for an active relation: for an alias,
/// posts with `antecedent` get it replaced by `add`; for an implication,
/// posts with `antecedent` that lack some of `add` get them. Returns how
/// many posts changed; call until it returns 0. The new post versions are
/// attributed to `relation_id`.
pub async fn apply_batch(
    db: &PgPool,
    relation_id: Option<i32>,
    kind: Kind,
    antecedent: i32,
    add: &[i32],
    limit: i64,
) -> sqlx::Result<u64> {
    let query = match kind {
        Kind::Alias => {
            "WITH batch AS (
                 SELECT id FROM posts WHERE tag_ids @> ARRAY[$1::int4]
                 ORDER BY id LIMIT $3 FOR UPDATE
             )
             UPDATE posts SET tag_ids = uniq(sort((posts.tag_ids - $1::int4) | $2::int4[])),
                              updated_at = now()
             FROM batch WHERE posts.id = batch.id"
        }
        Kind::Implication => {
            "WITH batch AS (
                 SELECT id FROM posts WHERE tag_ids @> ARRAY[$1::int4] AND NOT tag_ids @> $2::int4[]
                 ORDER BY id LIMIT $3 FOR UPDATE
             )
             UPDATE posts SET tag_ids = uniq(sort(posts.tag_ids | $2::int4[])), updated_at = now()
             FROM batch WHERE posts.id = batch.id"
        }
    };
    let mut tx = db.begin().await?;
    crate::post_versions::attribute(&mut tx, None, relation_id).await?;
    let result = sqlx::query(query)
        .bind(antecedent)
        .bind(add)
        .bind(limit)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    async fn admin(pool: &PgPool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'admin', id FROM roles WHERE system_key = 'admin' RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn ask(pool: &PgPool, kind: Kind, a: &str, c: &str) -> Result<i32, RelationError> {
        request(
            pool,
            NewRequest {
                kind,
                antecedent: a,
                consequent: c,
                reason: "",
                creator_id: None,
            },
        )
        .await
    }

    /// Requests and approves, returning the rule error if either fails.
    async fn add(pool: &PgPool, kind: Kind, a: &str, c: &str) -> Result<i32, RuleError> {
        let by = sqlx::query_scalar("SELECT id FROM users WHERE name = 'admin'")
            .fetch_one(pool)
            .await
            .unwrap();
        let rule = |e: RelationError| match e {
            RelationError::Rule(rule) => rule,
            RelationError::Db(e) => panic!("{e}"),
        };
        let id = ask(pool, kind, a, c).await.map_err(rule)?;
        approve(pool, id, by).await.map_err(rule)?;
        Ok(id)
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn alias_rules(pool: PgPool) {
        admin(&pool).await;
        add(&pool, Kind::Alias, "kitty", "cat").await.unwrap();
        assert_eq!(
            add(&pool, Kind::Alias, "kitty", "feline").await,
            Err(RuleError::AlreadyAliased("kitty".into(), "cat".into()))
        );
        assert_eq!(
            add(&pool, Kind::Alias, "kitten", "kitty").await,
            Err(RuleError::AliasedAway("kitty".into(), "cat".into()))
        );
        // Aliasing the target repoints existing aliases: no chains.
        add(&pool, Kind::Alias, "cat", "feline").await.unwrap();
        assert_eq!(
            aliases_of(&pool, &["kitty", "cat", "feline"])
                .await
                .unwrap()
                .len(),
            2
        );
        let mut targets: Vec<String> = aliases_of(&pool, &["kitty", "cat"])
            .await
            .unwrap()
            .into_iter()
            .map(|(_, c)| c)
            .collect();
        targets.dedup();
        assert_eq!(targets, ["feline"]);

        add(&pool, Kind::Implication, "tabby", "feline")
            .await
            .unwrap();
        assert_eq!(
            add(&pool, Kind::Alias, "tabby", "stripes").await,
            Err(RuleError::HasImplications("tabby".into()))
        );
        assert_eq!(
            ask(&pool, Kind::Alias, "dog", "cat")
                .await
                .err()
                .map(|e| e.to_string()),
            Some("`cat` is aliased to `feline`; use `feline` instead.".into())
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn implication_rules(pool: PgPool) {
        admin(&pool).await;
        add(&pool, Kind::Implication, "a", "b").await.unwrap();
        add(&pool, Kind::Implication, "b", "c").await.unwrap();
        assert_eq!(implied_by(&pool, &["a"]).await.unwrap(), ["b", "c"]);
        assert_eq!(
            add(&pool, Kind::Implication, "c", "a").await,
            Err(RuleError::Cycle("c".into(), "a".into()))
        );
        assert_eq!(
            add(&pool, Kind::Implication, "a", "c").await,
            Err(RuleError::Redundant("a".into(), "c".into()))
        );
        assert_eq!(
            add(&pool, Kind::Implication, "a", "b").await,
            Err(RuleError::Duplicate)
        );
        add(&pool, Kind::Alias, "x", "y").await.unwrap();
        assert_eq!(
            add(&pool, Kind::Implication, "x", "a").await,
            Err(RuleError::AliasedAway("x".into(), "y".into()))
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn approval_rechecks_rules(pool: PgPool) {
        let by = admin(&pool).await;
        // Both fine on their own, but together a loop.
        let first = ask(&pool, Kind::Implication, "a", "b").await.unwrap();
        let second = ask(&pool, Kind::Implication, "b", "a").await.unwrap();
        approve(&pool, first, by).await.unwrap();
        assert!(matches!(
            approve(&pool, second, by).await,
            Err(RelationError::Rule(RuleError::Cycle(..)))
        ));
        assert!(matches!(
            approve(&pool, first, by).await,
            Err(RelationError::Rule(RuleError::NotPending))
        ));

        reject(&pool, second, by).await.unwrap();
        let rejected = by_id(&pool, second).await.unwrap().unwrap();
        assert_eq!(rejected.status, Status::Rejected);
        assert_eq!(rejected.approver_name.as_deref(), Some("admin"));
        assert!(remove(&pool, first, by).await.unwrap());
        assert!(!remove(&pool, first, by).await.unwrap());
        assert!(implied_by(&pool, &["a"]).await.unwrap().is_empty());

        let jobs: i64 =
            sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'tags.apply_relation'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(jobs, 1);

        let listed = list(&pool, Kind::Implication, None, "", 0, 10)
            .await
            .unwrap();
        assert_eq!(
            listed.iter().map(|r| r.id).collect::<Vec<_>>(),
            [second, first]
        );
        let pending = list(&pool, Kind::Implication, Some(Status::Pending), "", 0, 10)
            .await
            .unwrap();
        assert!(pending.is_empty());
        assert!(
            list(&pool, Kind::Alias, None, "", 0, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn batches_rewrite_posts(pool: PgPool) {
        let ids = crate::tags::tests::create(&pool, &["old", "new", "extra", "other"]).await;
        let [old, new, extra, other] = ids[..] else {
            unreachable!()
        };
        for tags in [vec![old, other], vec![old], vec![other]] {
            let mut tags = tags;
            tags.sort_unstable();
            sqlx::query("INSERT INTO posts (rating, tag_ids) VALUES ('g', $1)")
                .bind(tags)
                .execute(&pool)
                .await
                .unwrap();
        }
        let mut add = vec![new, extra];
        add.sort_unstable();
        let mut total = 0;
        loop {
            let n = apply_batch(&pool, None, Kind::Alias, old, &add, 1)
                .await
                .unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
        assert_eq!(total, 2);
        let count = |name: &'static str| {
            let pool = pool.clone();
            async move {
                crate::tags::by_name(&pool, name)
                    .await
                    .unwrap()
                    .unwrap()
                    .post_count
            }
        };
        assert_eq!(
            (count("old").await, count("new").await, count("extra").await),
            (0, 2, 2)
        );

        // Implication: posts with `other` gain `extra`, once.
        for _ in 0..2 {
            while apply_batch(&pool, None, Kind::Implication, other, &[extra], 10)
                .await
                .unwrap()
                > 0
            {}
        }
        assert_eq!(count("extra").await, 3);
    }
}
