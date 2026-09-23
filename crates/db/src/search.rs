//! Running searches: from a parsed [`Query`] to post ids and counts.
//!
//! [`Plan::resolve`] looks up what the query names (tags through aliases,
//! wildcard expansions, users) and works out which statuses the viewer
//! gets. [`Plan::ids`] then builds the SQL.
//!
//! Tag terms become intarray operators on `posts.tag_ids`, which the GIN
//! index serves. The catch is `ORDER BY id DESC LIMIT n`: Postgres can
//! either walk the id index, checking each post's tags until it has n
//! matches, or collect every match through the GIN index and sort. Walking
//! wins when matches are common and loses badly when they are rare, and
//! Postgres' guess about which applies is often wrong for tag
//! combinations. We know each tag's exact post count, so [`Plan`] makes
//! that choice itself and writes the SQL so Postgres can only do that.

use std::str::FromStr;

use sqlx::{PgPool, Postgres, QueryBuilder};
use uwuu_core::config::SearchConfig;
use uwuu_core::posts::PostStatus;
use uwuu_core::search::{
    Bound, Condition, Filter, Order, ParentFilter, Query, StatusFilter, TagTerm,
};

use crate::posts::Visibility;
use crate::tag_relations;

/// Which page of results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageRef {
    /// 1-based.
    Number(u32),
    /// Posts with lower ids than this one (`b123`), for id order.
    Before(i64),
    /// Posts with higher ids than this one (`a123`), for id order.
    After(i64),
}

impl Default for PageRef {
    fn default() -> Self {
        PageRef::Number(1)
    }
}

impl FromStr for PageRef {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let id = |rest: &str| rest.parse::<i64>().ok().filter(|&id| id > 0).ok_or(());
        if let Some(rest) = s.strip_prefix('b') {
            return id(rest).map(PageRef::Before);
        }
        if let Some(rest) = s.strip_prefix('a') {
            return id(rest).map(PageRef::After);
        }
        s.parse::<u32>()
            .ok()
            .filter(|&n| n > 0)
            .map(PageRef::Number)
            .ok_or(())
    }
}

/// How many posts a search matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    Exact(i64),
    /// From tag post counts or table statistics.
    About(i64),
    /// Counting stopped here.
    AtLeast(i64),
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// Something about the query the user should change.
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// A set of tag ids a post must have at least one of, and roughly how
/// many posts that is.
#[derive(Debug, Clone, PartialEq)]
struct TagSet {
    ids: Vec<i32>,
    posts: i64,
}

/// How to get posts in order; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    /// No tag conditions to worry about: let Postgres choose.
    Natural,
    /// Walk the index of the sort order, filtering on tags.
    Walk,
    /// Collect matches through the tag index, then sort.
    Collect,
}

#[derive(Debug, Clone)]
pub struct Plan {
    /// Known to match nothing (a required tag doesn't exist, …).
    nothing: bool,
    /// Tags that must all be present.
    required: Vec<TagSet>,
    /// Tags of which at least one must be present.
    any: Option<TagSet>,
    excluded: Vec<i32>,
    conditions: Vec<Condition>,
    /// `user:` filters resolved to ids.
    uploaders: Vec<(bool, i64)>,
    statuses: Vec<&'static str>,
    /// The viewer, if their own pending posts are included.
    own_pending: Option<i64>,
    order: Order,
    per_page: u32,
    /// Estimated number of posts, for choosing a strategy.
    total: f64,
    max_page: u32,
    count_limit: u32,
}

impl Plan {
    /// Resolves `query` for a viewer who may see `visibility`.
    pub async fn resolve(
        db: &PgPool,
        query: &Query,
        visibility: &Visibility,
        config: &SearchConfig,
    ) -> Result<Self, SearchError> {
        if query.term_count() > config.max_terms {
            return Err(SearchError::Invalid(format!(
                "Searches may have at most {} tags and filters.",
                config.max_terms
            )));
        }
        if let Some(limit) = query.limit
            && limit > config.max_per_page
        {
            return Err(SearchError::Invalid(format!(
                "limit: may be at most {}.",
                config.max_per_page
            )));
        }
        let (statuses, own_pending) = statuses(query, visibility);
        let mut plan = Plan {
            nothing: statuses.is_empty() && own_pending.is_none(),
            required: Vec::new(),
            any: None,
            excluded: Vec::new(),
            conditions: Vec::new(),
            uploaders: Vec::new(),
            statuses,
            own_pending,
            order: query.order.unwrap_or_default(),
            per_page: query.limit.unwrap_or(config.per_page),
            total: 0.0,
            max_page: config.max_page,
            count_limit: config.count_limit,
        };

        for term in &query.all {
            let set = expand(db, term, config.wildcard_limit).await?;
            if set.ids.is_empty() {
                plan.nothing = true;
            }
            plan.required.push(set);
        }
        if !query.any.is_empty() {
            let mut union = TagSet {
                ids: Vec::new(),
                posts: 0,
            };
            for term in &query.any {
                let set = expand(db, term, config.wildcard_limit).await?;
                union.ids.extend(set.ids);
                union.posts += set.posts;
            }
            if union.ids.is_empty() {
                plan.nothing = true;
            }
            plan.any = Some(union);
        }
        for term in &query.none {
            plan.excluded
                .extend(expand(db, term, config.wildcard_limit).await?.ids);
        }
        for set in plan.required.iter_mut().chain(plan.any.as_mut()) {
            set.ids.sort_unstable();
            set.ids.dedup();
        }
        plan.excluded.sort_unstable();
        plan.excluded.dedup();

        for condition in &query.conditions {
            match &condition.filter {
                // Already folded into `statuses`.
                Filter::Status(_) => {}
                Filter::User(name) => {
                    match crate::users::by_name(db, name).await? {
                        Some(user) => plan.uploaders.push((condition.negated, user.id)),
                        // Nobody by that name uploaded anything.
                        None if !condition.negated => plan.nothing = true,
                        None => {}
                    }
                }
                _ => plan.conditions.push(condition.clone()),
            }
        }

        if !plan.nothing {
            plan.total = sqlx::query_scalar::<_, f32>(
                "SELECT reltuples FROM pg_class WHERE oid = 'posts'::regclass",
            )
            .fetch_one(db)
            .await?
            .max(0.0)
            .into();
        }
        Ok(plan)
    }

    /// Posts per page.
    pub fn per_page(&self) -> u32 {
        self.per_page
    }

    pub fn order(&self) -> Order {
        self.order
    }

    /// Whether `page=b…` / `page=a…` work for this search.
    pub fn supports_keyset(&self) -> bool {
        matches!(self.order, Order::IdDesc | Order::IdAsc)
    }

    /// Positive tag conditions: what the strategy is about.
    fn tag_sets(&self) -> impl Iterator<Item = &TagSet> {
        self.required.iter().chain(self.any.as_ref())
    }

    /// The expected number of matches, assuming tags are independent.
    fn expected_matches(&self) -> f64 {
        let total = self.total.max(1.0);
        self.tag_sets().fold(total, |matches, set| {
            matches * (set.posts as f64 / total).min(1.0)
        })
    }

    fn strategy(&self, offset: u32) -> Strategy {
        let indexed_order = matches!(
            self.order,
            Order::IdDesc
                | Order::IdAsc
                | Order::ScoreDesc
                | Order::ScoreAsc
                | Order::FavCountDesc
                | Order::FavCountAsc
        );
        if self.tag_sets().next().is_none() || !indexed_order {
            return Strategy::Natural;
        }
        // Walking reads about (offset + limit) × total / matches posts;
        // collecting reads every match. The factor leans towards
        // collecting, because tags that go together less than chance
        // make walks longer than expected.
        let matches = self.expected_matches();
        let wanted = f64::from(offset) + f64::from(self.per_page);
        if wanted * self.total * 4.0 < matches * matches {
            Strategy::Walk
        } else {
            Strategy::Collect
        }
    }

    /// The ids of the posts on `page`, in order.
    pub async fn ids(&self, db: &PgPool, page: PageRef) -> Result<Vec<i64>, SearchError> {
        let mut query = self.ids_query(page)?;
        if self.nothing {
            return Ok(Vec::new());
        }
        let mut ids: Vec<i64> = query.build_query_scalar().fetch_all(db).await?;
        // Keyset pages against the display order are fetched nearest
        // first, i.e. backwards.
        if matches!(
            (page, self.order),
            (PageRef::After(_), Order::IdDesc) | (PageRef::Before(_), Order::IdAsc)
        ) {
            ids.reverse();
        }
        Ok(ids)
    }

    fn ids_query(&self, page: PageRef) -> Result<QueryBuilder<Postgres>, SearchError> {
        let offset = match page {
            PageRef::Number(n) if n > self.max_page => {
                return Err(SearchError::Invalid(format!(
                    "Numbered pages stop at {}; use the next and previous links to go further.",
                    self.max_page
                )));
            }
            PageRef::Number(n) => (n - 1).saturating_mul(self.per_page),
            _ if !self.supports_keyset() => {
                return Err(SearchError::Invalid(
                    "Those page links only work when sorting by id.".into(),
                ));
            }
            _ => 0,
        };
        let strategy = self.strategy(offset);
        let mut sql = QueryBuilder::new("SELECT p.id FROM posts p");
        self.push_from_where(&mut sql, strategy);

        // Keyset pages walk away from the given id, so `a…` pages in
        // descending order (and `b…` pages in ascending order) are fetched
        // in reverse and flipped afterwards.
        let descending = match (page, self.order) {
            (PageRef::Before(id), _) => {
                sql.push(" AND p.id < ").push_bind(id);
                true
            }
            (PageRef::After(id), _) => {
                sql.push(" AND p.id > ").push_bind(id);
                false
            }
            (_, order) => order != Order::IdAsc,
        };
        sql.push(" ORDER BY ");
        let collect = strategy == Strategy::Collect;
        // `+ 0` hides the column from the planner so it can't pick the
        // order's index (and a walk) behind our back.
        let hide = if collect { " + 0" } else { "" };
        match self.order {
            Order::IdDesc | Order::IdAsc => {
                let direction = if descending { "DESC" } else { "ASC" };
                sql.push(format!("p.id{hide} {direction}"));
            }
            Order::ScoreDesc => {
                sql.push(format!("p.score{hide} DESC, p.id DESC"));
            }
            Order::ScoreAsc => {
                sql.push(format!("p.score{hide} ASC, p.id ASC"));
            }
            Order::FavCountDesc => {
                sql.push(format!("p.fav_count{hide} DESC, p.id DESC"));
            }
            Order::FavCountAsc => {
                sql.push(format!("p.fav_count{hide} ASC, p.id ASC"));
            }
            Order::MpixelsDesc => {
                sql.push("a.width::bigint * a.height DESC, p.id DESC");
            }
            Order::MpixelsAsc => {
                sql.push("a.width::bigint * a.height ASC, p.id ASC");
            }
            Order::FileSizeDesc => {
                sql.push("a.file_size DESC, p.id DESC");
            }
            Order::FileSizeAsc => {
                sql.push("a.file_size ASC, p.id ASC");
            }
            Order::Landscape => {
                sql.push("a.width::float8 / a.height DESC, p.id DESC");
            }
            Order::Portrait => {
                sql.push("a.width::float8 / a.height ASC, p.id DESC");
            }
            Order::DurationDesc => {
                sql.push("a.duration_ms DESC NULLS LAST, p.id DESC");
            }
            Order::DurationAsc => {
                sql.push("a.duration_ms ASC NULLS LAST, p.id ASC");
            }
            Order::TagCountDesc => {
                sql.push("p.tag_count DESC, p.id DESC");
            }
            Order::TagCountAsc => {
                sql.push("p.tag_count ASC, p.id ASC");
            }
            Order::Random => {
                sql.push("random()");
            }
        }
        sql.push(" LIMIT ").push_bind(i64::from(self.per_page));
        if offset > 0 {
            sql.push(" OFFSET ").push_bind(i64::from(offset));
        }
        Ok(sql)
    }

    /// Whether any condition or the order needs `media_assets`.
    fn needs_media(&self) -> bool {
        let media_order = matches!(
            self.order,
            Order::MpixelsDesc
                | Order::MpixelsAsc
                | Order::FileSizeDesc
                | Order::FileSizeAsc
                | Order::Landscape
                | Order::Portrait
                | Order::DurationDesc
                | Order::DurationAsc
        );
        media_order
            || self.conditions.iter().any(|c| {
                matches!(
                    c.filter,
                    Filter::Width(_)
                        | Filter::Height(_)
                        | Filter::Mpixels(_)
                        | Filter::Ratio(_)
                        | Filter::FileSize(_)
                        | Filter::Duration(_)
                        | Filter::FileType(_)
                        | Filter::Md5(_)
                )
            })
    }

    /// Everything after `SELECT … FROM posts p`: joins and the WHERE clause.
    fn push_from_where(&self, sql: &mut QueryBuilder<Postgres>, strategy: Strategy) {
        if self.needs_media() {
            sql.push(" JOIN media_assets a ON a.post_id = p.id");
        }
        sql.push(" WHERE (p.status = ANY(")
            .push_bind(self.statuses.clone())
            .push(")");
        if let Some(viewer) = self.own_pending {
            sql.push(" OR (p.status = 'pending' AND p.uploader_id = ")
                .push_bind(viewer)
                .push(")");
        }
        sql.push(")");

        // Walks must not use the tag index: `IS TRUE` makes the condition
        // one Postgres can't match to an index.
        let (open, close) = if strategy == Strategy::Walk {
            ("(", ") IS TRUE")
        } else {
            ("", "")
        };
        let single: Vec<i32> = self
            .required
            .iter()
            .filter(|set| set.ids.len() == 1)
            .map(|set| set.ids[0])
            .collect();
        if !single.is_empty() {
            let mut single = single;
            single.sort_unstable();
            sql.push(format!(" AND {open}p.tag_ids @> "))
                .push_bind(single)
                .push(format!("::int4[]{close}"));
        }
        for set in self
            .required
            .iter()
            .filter(|set| set.ids.len() > 1)
            .chain(self.any.as_ref())
        {
            sql.push(format!(" AND {open}p.tag_ids && "))
                .push_bind(set.ids.clone())
                .push(format!("::int4[]{close}"));
        }
        if !self.excluded.is_empty() {
            sql.push(" AND NOT p.tag_ids && ")
                .push_bind(self.excluded.clone())
                .push("::int4[]");
        }
        for (negated, uploader) in &self.uploaders {
            sql.push(if *negated {
                " AND p.uploader_id IS DISTINCT FROM "
            } else {
                " AND p.uploader_id = "
            })
            .push_bind(*uploader);
        }
        for condition in &self.conditions {
            sql.push(" AND (");
            push_filter(sql, &condition.filter);
            // NOT would let NULLs (videos' missing durations, …) through as
            // unknown; IS NOT TRUE counts them as not matching.
            sql.push(if condition.negated {
                ") IS NOT TRUE"
            } else {
                ")"
            });
        }
    }

    /// How many posts match. Exact up to the count limit; beyond it, a
    /// single tag's post count or the table size where those apply.
    pub async fn count(&self, db: &PgPool) -> Result<Count, SearchError> {
        if self.nothing {
            return Ok(Count::Exact(0));
        }
        let limit = i64::from(self.count_limit);
        let unfiltered = self.conditions.is_empty()
            && self.uploaders.is_empty()
            && self.excluded.is_empty()
            && self.any.is_none();
        let shortcut = match &self.required[..] {
            [] if unfiltered => Some(self.total as i64),
            [set] if unfiltered && set.ids.len() == 1 => Some(set.posts),
            _ => None,
        };
        if let Some(estimate) = shortcut.filter(|&n| n > limit) {
            return Ok(Count::About(estimate));
        }
        let mut sql = QueryBuilder::new("SELECT count(*) FROM (SELECT 1 FROM posts p");
        self.push_from_where(&mut sql, Strategy::Natural);
        sql.push(" LIMIT ").push_bind(limit + 1).push(") AS hits");
        let n: i64 = sql.build_query_scalar().fetch_one(db).await?;
        Ok(if n > limit {
            Count::AtLeast(limit)
        } else {
            Count::Exact(n)
        })
    }
}

/// The statuses a search covers, and the viewer if their own pending
/// posts are included. Without a `status:` filter, deleted posts are left
/// out even for those who may see them.
fn statuses(query: &Query, visibility: &Visibility) -> (Vec<&'static str>, Option<i64>) {
    let visible = &visibility.statuses;
    let mut wanted: Vec<PostStatus> = match query.status() {
        None => visible
            .iter()
            .copied()
            .filter(|s| *s != PostStatus::Deleted)
            .collect(),
        Some(StatusFilter::Any) => visible.clone(),
        Some(status) => {
            let status: PostStatus = status
                .as_str()
                .parse()
                .expect("status filters name post statuses");
            visible.iter().copied().filter(|s| *s == status).collect()
        }
    };
    let mut own_pending = matches!(
        query.status(),
        None | Some(StatusFilter::Any) | Some(StatusFilter::Pending)
    );
    for condition in &query.conditions {
        if let (true, Filter::Status(status)) = (condition.negated, &condition.filter) {
            wanted.retain(|s| s.as_str() != status.as_str());
            if *status == StatusFilter::Pending {
                own_pending = false;
            }
        }
    }
    let own_pending = visibility
        .viewer
        .filter(|_| own_pending && !wanted.contains(&PostStatus::Pending));
    (wanted.iter().map(|s| s.as_str()).collect(), own_pending)
}

/// The tag ids a term stands for: its tag (after aliases), or a
/// wildcard's most used matches.
async fn expand(db: &PgPool, term: &TagTerm, wildcard_limit: u32) -> sqlx::Result<TagSet> {
    let rows: Vec<(i32, i32)> = match term {
        TagTerm::Name(name) => {
            let aliased = tag_relations::aliases_of(db, &[name.as_str()]).await?;
            let name = aliased
                .first()
                .map_or(name.as_str(), |(_, consequent)| consequent.as_str());
            sqlx::query_as("SELECT id, post_count FROM tags WHERE name = $1")
                .bind(name)
                .fetch_all(db)
                .await?
        }
        TagTerm::Wildcard(pattern) => {
            let like = crate::tags::like_pattern(pattern);
            sqlx::query_as(
                "SELECT id, post_count FROM tags WHERE name LIKE $1 AND post_count > 0
                 ORDER BY post_count DESC LIMIT $2",
            )
            .bind(like)
            .bind(i64::from(wildcard_limit))
            .fetch_all(db)
            .await?
        }
    };
    Ok(TagSet {
        ids: rows.iter().map(|(id, _)| *id).collect(),
        posts: rows.iter().map(|(_, n)| i64::from(*n)).sum(),
    })
}

fn push_filter(sql: &mut QueryBuilder<Postgres>, filter: &Filter) {
    match filter {
        Filter::Id(b) => push_bound(sql, "p.id", b),
        Filter::Score(b) => push_bound(sql, "p.score", b),
        Filter::FavCount(b) => push_bound(sql, "p.fav_count", b),
        Filter::TagCount(b) => push_bound(sql, "p.tag_count", b),
        Filter::Width(b) => push_bound(sql, "a.width", b),
        Filter::Height(b) => push_bound(sql, "a.height", b),
        Filter::FileSize(b) => push_bound(sql, "a.file_size", b),
        Filter::Mpixels(b) => {
            push_float_bound(sql, "(a.width::float8 * a.height / 1000000)", b, 0.05);
        }
        Filter::Ratio(b) => push_float_bound(sql, "(a.width::float8 / a.height)", b, 0.01),
        Filter::Duration(b) => {
            push_float_bound(sql, "(a.duration_ms::float8 / 1000)", b, 0.5);
        }
        Filter::Rating(ratings) => {
            let codes: Vec<&str> = ratings.iter().map(|r| r.code()).collect();
            sql.push("p.rating = ANY(").push_bind(codes).push(")");
        }
        Filter::FileType(types) => {
            sql.push("a.media_type = ANY(")
                .push_bind(types.clone())
                .push(")");
        }
        Filter::Md5(hashes) => {
            let hashes: Vec<Vec<u8>> = hashes.iter().map(|h| h.to_vec()).collect();
            sql.push("a.md5 = ANY(").push_bind(hashes).push(")");
        }
        Filter::Date { from, until } => {
            let midnight = |d: &time::Date| d.midnight().assume_utc();
            sql.push("TRUE");
            if let Some(from) = from {
                sql.push(" AND p.created_at >= ").push_bind(midnight(from));
            }
            if let Some(until) = until {
                sql.push(" AND p.created_at < ").push_bind(midnight(until));
            }
        }
        Filter::Parent(ParentFilter::None) => {
            sql.push("p.parent_id IS NULL");
        }
        Filter::Parent(ParentFilter::Any) => {
            sql.push("p.parent_id IS NOT NULL");
        }
        Filter::Parent(ParentFilter::Of(id)) => {
            sql.push("p.parent_id = ")
                .push_bind(*id)
                .push(" OR p.id = ")
                .push_bind(*id);
        }
        Filter::Status(_) | Filter::User(_) => {
            unreachable!("resolved in Plan::resolve")
        }
    }
}

fn push_bound(sql: &mut QueryBuilder<Postgres>, column: &str, bound: &Bound<i64>) {
    sql.push(column);
    match bound {
        Bound::Eq(v) => sql.push(" = ").push_bind(*v),
        Bound::Lt(v) => sql.push(" < ").push_bind(*v),
        Bound::Le(v) => sql.push(" <= ").push_bind(*v),
        Bound::Gt(v) => sql.push(" > ").push_bind(*v),
        Bound::Ge(v) => sql.push(" >= ").push_bind(*v),
        Bound::Between(a, b) => sql
            .push(" BETWEEN ")
            .push_bind(*a)
            .push(" AND ")
            .push_bind(*b),
        Bound::In(values) => sql.push(" = ANY(").push_bind(values.clone()).push(")"),
    };
}

/// Like [`push_bound`], with equality meaning within `tolerance`.
fn push_float_bound(
    sql: &mut QueryBuilder<Postgres>,
    expression: &str,
    bound: &Bound<f64>,
    tolerance: f64,
) {
    let near = |sql: &mut QueryBuilder<Postgres>, v: f64| {
        sql.push(expression)
            .push(" BETWEEN ")
            .push_bind(v - tolerance)
            .push(" AND ")
            .push_bind(v + tolerance);
    };
    match bound {
        Bound::Eq(v) => near(sql, *v),
        Bound::In(values) => {
            for (i, v) in values.iter().enumerate() {
                if i > 0 {
                    sql.push(" OR ");
                }
                near(sql, *v);
            }
        }
        Bound::Lt(v) => {
            sql.push(expression).push(" < ").push_bind(*v);
        }
        Bound::Le(v) => {
            sql.push(expression).push(" <= ").push_bind(*v);
        }
        Bound::Gt(v) => {
            sql.push(expression).push(" > ").push_bind(*v);
        }
        Bound::Ge(v) => {
            sql.push(expression).push(" >= ").push_bind(*v);
        }
        Bound::Between(a, b) => {
            sql.push(expression)
                .push(" BETWEEN ")
                .push_bind(*a)
                .push(" AND ")
                .push_bind(*b);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use sqlx::{Execute, PgPool};
    use uwuu_core::posts::PostStatus;

    use super::*;
    use crate::tags::WantedTag;

    /// A post to seed; unset fields get plain defaults.
    #[derive(Clone)]
    struct Seed {
        tags: &'static [&'static str],
        status: &'static str,
        rating: &'static str,
        score: i32,
        size: (i32, i32),
        media_type: &'static str,
        file_size: i64,
        duration_ms: Option<i32>,
        uploader: Option<i64>,
        parent: Option<i64>,
        days_ago: i64,
    }

    impl Default for Seed {
        fn default() -> Self {
            Seed {
                tags: &[],
                status: "active",
                rating: "g",
                score: 0,
                size: (100, 100),
                media_type: "png",
                file_size: 1000,
                duration_ms: None,
                uploader: None,
                parent: None,
                days_ago: 0,
            }
        }
    }

    async fn seed(pool: &PgPool, post: Seed) -> i64 {
        let mut conn = pool.acquire().await.unwrap();
        let wanted: Vec<WantedTag<'_>> = post
            .tags
            .iter()
            .map(|name| WantedTag {
                name,
                category_id: None,
            })
            .collect();
        let mut tag_ids: Vec<i32> = crate::tags::ensure(&mut conn, &wanted, false)
            .await
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();
        tag_ids.sort_unstable();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, status, score, tag_ids, uploader_id, parent_id, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, now() - make_interval(days => $7::int))
             RETURNING id",
        )
        .bind(post.rating)
        .bind(post.status)
        .bind(post.score)
        .bind(tag_ids)
        .bind(post.uploader)
        .bind(post.parent)
        .bind(post.days_ago as i32)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        let mut sha256 = [0u8; 32];
        sha256[..8].copy_from_slice(&id.to_be_bytes());
        let mut md5 = [0u8; 16];
        md5[..8].copy_from_slice(&id.to_be_bytes());
        sqlx::query(
            "INSERT INTO media_assets
                 (post_id, sha256, md5, media_type, width, height, duration_ms, file_size, storage_key)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'k')",
        )
        .bind(id)
        .bind(&sha256[..])
        .bind(&md5[..])
        .bind(post.media_type)
        .bind(post.size.0)
        .bind(post.size.1)
        .bind(post.duration_ms)
        .bind(post.file_size)
        .execute(&mut *conn)
        .await
        .unwrap();
        id
    }

    fn public() -> Visibility {
        Visibility {
            statuses: vec![PostStatus::Active, PostStatus::Flagged],
            viewer: None,
        }
    }

    async fn search_as(pool: &PgPool, input: &str, visibility: &Visibility) -> Vec<i64> {
        let query = Query::parse(input).unwrap();
        let plan = Plan::resolve(pool, &query, visibility, &SearchConfig::default())
            .await
            .unwrap();
        plan.ids(pool, PageRef::default()).await.unwrap()
    }

    async fn search(pool: &PgPool, input: &str) -> Vec<i64> {
        search_as(pool, input, &public()).await
    }

    #[test]
    fn page_refs() {
        assert_eq!("3".parse(), Ok(PageRef::Number(3)));
        assert_eq!("b120".parse(), Ok(PageRef::Before(120)));
        assert_eq!("a7".parse(), Ok(PageRef::After(7)));
        for bad in ["0", "-1", "b", "bx", "a0", "x1"] {
            assert!(bad.parse::<PageRef>().is_err(), "{bad}");
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn tag_terms(pool: PgPool) {
        let cat = seed(
            &pool,
            Seed {
                tags: &["cat", "cute", "long_hair"],
                ..Seed::default()
            },
        )
        .await;
        let dog = seed(
            &pool,
            Seed {
                tags: &["dog", "cute", "short_hair"],
                ..Seed::default()
            },
        )
        .await;
        let both = seed(
            &pool,
            Seed {
                tags: &["cat", "dog"],
                ..Seed::default()
            },
        )
        .await;
        let none = seed(&pool, Seed::default()).await;

        assert_eq!(search(&pool, "").await, [none, both, dog, cat]);
        assert_eq!(search(&pool, "cat").await, [both, cat]);
        assert_eq!(search(&pool, "cat dog").await, [both]);
        assert_eq!(search(&pool, "cat -dog").await, [cat]);
        assert_eq!(search(&pool, "~cat ~dog -cute").await, [both]);
        assert_eq!(search(&pool, "~cat ~nonexistent").await, [both, cat]);
        assert_eq!(search(&pool, "*_hair").await, [dog, cat]);
        assert_eq!(search(&pool, "-*_hair").await, [none, both]);
        assert_eq!(search(&pool, "cat -nonexistent").await, [both, cat]);
        assert!(search(&pool, "cat nonexistent").await.is_empty());
        assert!(search(&pool, "nothing_*").await.is_empty());
        assert!(search(&pool, "~nonexistent").await.is_empty());

        // Searches follow aliases.
        sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('alias', 'kitty', 'cat', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(search(&pool, "kitty").await, [both, cat]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn filters(pool: PgPool) {
        let uploader: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'Alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let wide = seed(
            &pool,
            Seed {
                rating: "e",
                score: 10,
                size: (1920, 1080),
                file_size: 2_000_000,
                uploader: Some(uploader),
                tags: &["a", "b", "c"],
                ..Seed::default()
            },
        )
        .await;
        let tall = seed(
            &pool,
            Seed {
                rating: "q",
                score: -2,
                size: (1000, 2000),
                media_type: "jpeg",
                days_ago: 40,
                parent: Some(wide),
                ..Seed::default()
            },
        )
        .await;
        let clip = seed(
            &pool,
            Seed {
                size: (640, 480),
                media_type: "webm",
                duration_ms: Some(30_000),
                file_size: 500_000,
                ..Seed::default()
            },
        )
        .await;

        let cases: &[(&str, &[i64])] = &[
            ("rating:e", &[wide]),
            ("rating:q,e", &[tall, wide]),
            ("-rating:e", &[clip, tall]),
            ("score:>0", &[wide]),
            ("score:<0", &[tall]),
            ("score:-2..0", &[clip, tall]),
            (&format!("id:{tall}"), &[tall]),
            (&format!("id:>{tall}"), &[clip]),
            ("width:>=1000", &[tall, wide]),
            ("height:2000", &[tall]),
            ("mpixels:>2", &[wide]),
            ("mpixels:2", &[tall]),
            ("ratio:16:9", &[wide]),
            ("ratio:<1", &[tall]),
            ("ratio:4:3,16:9", &[clip, wide]),
            ("filesize:>1mb", &[wide]),
            ("filesize:500000", &[clip]),
            ("duration:>10", &[clip]),
            ("-duration:>10", &[tall, wide]),
            ("filetype:jpg", &[tall]),
            ("filetype:png,webm", &[clip, wide]),
            ("tagcount:3", &[wide]),
            ("tagcount:0", &[clip, tall]),
            ("user:alice", &[wide]),
            ("-user:alice", &[clip, tall]),
            ("user:nobody", &[]),
            ("-user:nobody", &[clip, tall, wide]),
            ("parent:none", &[clip, wide]),
            ("parent:any", &[tall]),
            (&format!("parent:{wide}"), &[tall, wide]),
            ("date:>=2000", &[clip, tall, wide]),
            ("date:<2000", &[]),
        ];
        for (input, expected) in cases {
            assert_eq!(&search(&pool, input).await, expected, "{input}");
        }

        let md5: Vec<u8> = sqlx::query_scalar("SELECT md5 FROM media_assets WHERE post_id = $1")
            .bind(clip)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            search(&pool, &format!("md5:{}", hex::encode(md5))).await,
            [clip]
        );
        let today = time::OffsetDateTime::now_utc().date();
        assert_eq!(
            search(&pool, &format!("date:{today}")).await.len(),
            2,
            "the post from 40 days ago is out"
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn statuses_and_visibility(pool: PgPool) {
        let viewer: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'me', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let active = seed(&pool, Seed::default()).await;
        let flagged = seed(
            &pool,
            Seed {
                status: "flagged",
                ..Seed::default()
            },
        )
        .await;
        let pending = seed(
            &pool,
            Seed {
                status: "pending",
                ..Seed::default()
            },
        )
        .await;
        let mine = seed(
            &pool,
            Seed {
                status: "pending",
                uploader: Some(viewer),
                ..Seed::default()
            },
        )
        .await;
        let deleted = seed(
            &pool,
            Seed {
                status: "deleted",
                ..Seed::default()
            },
        )
        .await;

        assert_eq!(search(&pool, "").await, [flagged, active]);
        assert!(search(&pool, "status:deleted").await.is_empty());
        let member = Visibility {
            viewer: Some(viewer),
            ..public()
        };
        assert_eq!(search_as(&pool, "", &member).await, [mine, flagged, active]);
        assert_eq!(search_as(&pool, "status:pending", &member).await, [mine]);
        assert_eq!(
            search_as(&pool, "-status:pending", &member).await,
            [flagged, active]
        );
        let staff = Visibility {
            statuses: vec![
                PostStatus::Active,
                PostStatus::Flagged,
                PostStatus::Pending,
                PostStatus::Deleted,
            ],
            viewer: None,
        };
        // Deleted posts only when asked for.
        assert_eq!(
            search_as(&pool, "", &staff).await,
            [mine, pending, flagged, active]
        );
        assert_eq!(search_as(&pool, "status:deleted", &staff).await, [deleted]);
        assert_eq!(
            search_as(&pool, "status:any", &staff).await,
            [deleted, mine, pending, flagged, active]
        );
        assert_eq!(
            search_as(&pool, "status:any -status:flagged -status:pending", &staff).await,
            [deleted, active]
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn orders_and_pages(pool: PgPool) {
        let mut ids = Vec::new();
        for (i, score) in [3, 1, 2, 5, 4].into_iter().enumerate() {
            ids.push(
                seed(
                    &pool,
                    Seed {
                        tags: &["x"],
                        score,
                        size: (100 + i as i32, 100),
                        ..Seed::default()
                    },
                )
                .await,
            );
        }
        let by = |order: &[usize]| order.iter().map(|&i| ids[i]).collect::<Vec<_>>();
        assert_eq!(search(&pool, "x order:score").await, by(&[3, 4, 0, 2, 1]));
        assert_eq!(
            search(&pool, "x order:score_asc").await,
            by(&[1, 2, 0, 4, 3])
        );
        assert_eq!(search(&pool, "order:id_asc").await, by(&[0, 1, 2, 3, 4]));
        assert_eq!(search(&pool, "order:landscape").await, by(&[4, 3, 2, 1, 0]));
        let mut random = search(&pool, "order:random").await;
        random.sort_unstable();
        assert_eq!(random, ids);

        let config = SearchConfig::default();
        let pages = |input: &'static str, page: PageRef| {
            let pool = pool.clone();
            let config = config.clone();
            async move {
                let query = Query::parse(input).unwrap();
                let plan = Plan::resolve(&pool, &query, &public(), &config)
                    .await
                    .unwrap();
                plan.ids(&pool, page).await
            }
        };
        assert_eq!(
            pages("x limit:2", PageRef::Number(2)).await.unwrap(),
            by(&[2, 1])
        );
        assert_eq!(
            pages("x limit:2", PageRef::Number(3)).await.unwrap(),
            by(&[0])
        );
        assert_eq!(
            pages("x limit:2", PageRef::Before(ids[3])).await.unwrap(),
            by(&[2, 1])
        );
        assert_eq!(
            pages("x limit:2", PageRef::After(ids[1])).await.unwrap(),
            by(&[3, 2])
        );
        assert_eq!(
            pages("x limit:2 order:id_asc", PageRef::After(ids[1]))
                .await
                .unwrap(),
            by(&[2, 3])
        );
        assert_eq!(
            pages("x limit:2 order:id_asc", PageRef::Before(ids[3]))
                .await
                .unwrap(),
            by(&[1, 2])
        );
        assert!(matches!(
            pages("x order:score", PageRef::Before(ids[3])).await,
            Err(SearchError::Invalid(_))
        ));
        assert!(matches!(
            pages("x", PageRef::Number(1001)).await,
            Err(SearchError::Invalid(_))
        ));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn limits_and_counts(pool: PgPool) {
        for _ in 0..5 {
            seed(
                &pool,
                Seed {
                    tags: &["x"],
                    ..Seed::default()
                },
            )
            .await;
        }
        seed(
            &pool,
            Seed {
                tags: &["y"],
                ..Seed::default()
            },
        )
        .await;
        let config = SearchConfig {
            count_limit: 3,
            max_terms: 2,
            ..SearchConfig::default()
        };
        let plan = |input: &'static str| {
            let pool = pool.clone();
            let config = config.clone();
            async move {
                let query = Query::parse(input).unwrap();
                Plan::resolve(&pool, &query, &public(), &config).await
            }
        };
        let count = |input: &'static str| {
            let plan = plan(input);
            let pool = pool.clone();
            async move { plan.await.unwrap().count(&pool).await.unwrap() }
        };
        assert_eq!(count("y").await, Count::Exact(1));
        assert_eq!(count("x rating:g").await, Count::AtLeast(3));
        // A single tag's count is known exactly…
        assert_eq!(count("x").await, Count::About(5));
        assert_eq!(count("missing").await, Count::Exact(0));
        assert!(matches!(
            plan("a b c").await,
            Err(SearchError::Invalid(message)) if message.contains("at most 2")
        ));
        assert!(matches!(
            plan("limit:1000").await,
            Err(SearchError::Invalid(message)) if message.contains("at most 200")
        ));
    }

    fn plan_with(total: f64, sets: &[i64], order: Order) -> Plan {
        Plan {
            nothing: false,
            required: sets
                .iter()
                .enumerate()
                .map(|(i, &posts)| TagSet {
                    ids: vec![i as i32 + 1],
                    posts,
                })
                .collect(),
            any: None,
            excluded: Vec::new(),
            conditions: Vec::new(),
            uploaders: Vec::new(),
            statuses: vec!["active"],
            own_pending: None,
            order,
            per_page: 40,
            total,
            max_page: 1000,
            count_limit: 10_000,
        }
    }

    #[test]
    fn strategy_follows_expected_matches() {
        let million = 1_000_000.0;
        // Common tag: walk. Rare tag: collect.
        assert_eq!(
            plan_with(million, &[300_000], Order::IdDesc).strategy(0),
            Strategy::Walk
        );
        assert_eq!(
            plan_with(million, &[500], Order::IdDesc).strategy(0),
            Strategy::Collect
        );
        // Two common tags that are rare together (by independence).
        assert_eq!(
            plan_with(million, &[20_000, 20_000], Order::IdDesc).strategy(0),
            Strategy::Collect
        );
        // Deep pages make walks longer.
        assert_eq!(
            plan_with(million, &[20_000], Order::IdDesc).strategy(0),
            Strategy::Walk
        );
        assert_eq!(
            plan_with(million, &[20_000], Order::IdDesc).strategy(40_000),
            Strategy::Collect
        );
        // Orders without an index sort anyway.
        assert_eq!(
            plan_with(million, &[300_000], Order::FileSizeDesc).strategy(0),
            Strategy::Natural
        );
        assert_eq!(
            plan_with(million, &[], Order::IdDesc).strategy(0),
            Strategy::Natural
        );
    }

    #[test]
    fn strategies_shape_the_sql() {
        let sql = |plan: Plan| {
            plan.ids_query(PageRef::default())
                .unwrap()
                .sql()
                .as_str()
                .to_owned()
        };
        let walk = sql(plan_with(1e6, &[300_000], Order::ScoreDesc));
        assert!(walk.contains("(p.tag_ids @> $2::int4[]) IS TRUE"), "{walk}");
        assert!(walk.contains("ORDER BY p.score DESC, p.id DESC"), "{walk}");
        let collect = sql(plan_with(1e6, &[500], Order::IdDesc));
        assert!(collect.contains("AND p.tag_ids @> $2::int4[]"), "{collect}");
        assert!(collect.contains("ORDER BY p.id + 0 DESC"), "{collect}");
    }

    /// Index names in the plan Postgres picks for `sql`.
    async fn plan_indexes(pool: &PgPool, plan: &Plan) -> Vec<String> {
        let mut query = plan.ids_query(PageRef::default()).unwrap();
        let sql = format!("EXPLAIN (FORMAT JSON) {}", query.sql().as_str());
        let arguments = query.build().take_arguments().unwrap().unwrap();
        let explained: Value = sqlx::query_scalar_with(sqlx::AssertSqlSafe(sql), arguments)
            .fetch_one(pool)
            .await
            .unwrap();
        let mut names = Vec::new();
        collect_index_names(&explained, &mut names);
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

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn postgres_follows_the_strategy(pool: PgPool) {
        // 20,000 posts: `common` on half of them, `rare` on 20.
        let common: i32 =
            sqlx::query_scalar("INSERT INTO tags (name) VALUES ('common') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let rare: i32 = sqlx::query_scalar("INSERT INTO tags (name) VALUES ('rare') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO posts (rating, tag_ids)
             SELECT 'g', CASE
                 WHEN i % 1000 = 0 THEN ARRAY[$1, $2]
                 WHEN i % 2 = 0 THEN ARRAY[$1]
                 ELSE '{}'::int4[] END
             FROM generate_series(1, 20000) AS i",
        )
        .bind(common)
        .bind(rare)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("ANALYZE posts").execute(&pool).await.unwrap();

        let resolve = |input: &'static str| {
            let pool = pool.clone();
            async move {
                let query = Query::parse(input).unwrap();
                Plan::resolve(&pool, &query, &public(), &SearchConfig::default())
                    .await
                    .unwrap()
            }
        };
        let walk = resolve("common").await;
        assert_eq!(walk.strategy(0), Strategy::Walk);
        assert_eq!(plan_indexes(&pool, &walk).await, ["posts_pkey"]);
        let collect = resolve("rare").await;
        assert_eq!(collect.strategy(0), Strategy::Collect);
        assert_eq!(plan_indexes(&pool, &collect).await, ["posts_tag_ids_idx"]);
        // And both return the right posts.
        assert_eq!(walk.ids(&pool, PageRef::default()).await.unwrap().len(), 40);
        assert_eq!(
            collect.ids(&pool, PageRef::default()).await.unwrap().len(),
            20
        );
    }
}
