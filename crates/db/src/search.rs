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

use futures_util::future::BoxFuture;
use futures_util::{StreamExt, TryStreamExt, stream};
use moekura_core::config::SearchConfig;
use moekura_core::posts::{PostStatus, Rating};
use moekura_core::search::{
    Age, Bound, CommentaryFilter, Expr, Filter, Order, ParentFilter, PixivFilter, PoolFilter,
    Query, RANK_DAYS, SourceFilter, StatusFilter, TagTerm, UserMatch, When,
};
use serde_json::Value as Json;
use sqlx::{PgPool, Postgres, QueryBuilder};

use crate::posts::Visibility;
use crate::tag_relations;

/// Most posts a `similar:` search considers.
const SIMILAR_LIMIT: i64 = 1000;

/// Most saved searches a `search:` term runs.
const SAVED_SEARCH_LIMIT: usize = 20;

/// Newest posts of each saved search a `search:` term includes.
const SAVED_SEARCH_POSTS: u32 = 500;

/// Saved searches a `search:` term runs at once, so it's quicker without
/// taking most of the pool.
const SAVED_SEARCH_CONCURRENCY: usize = 4;

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

/// Posts to leave out of a search, such as those a line of the viewer's
/// blacklist matches: those with all of `tags`, none of `not_tags`, a
/// rating in each of `ratings` and none in `not_ratings`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exclusion {
    pub tags: Vec<i32>,
    pub not_tags: Vec<i32>,
    pub ratings: Vec<Vec<Rating>>,
    pub not_ratings: Vec<Rating>,
}

/// What a user did to a post, in a [`Node::ByUser`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    Uploaded,
    Approved,
    Favorited,
    /// Wrote a comment that isn't deleted.
    Commented,
    /// Wrote or edited a note.
    Noted,
    Flagged,
    Upvoted,
    Downvoted,
}

/// A filter or group with the names in it looked up, ready for SQL.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    /// Matches every post, or none.
    Const(bool),
    /// Has at least one of these tags.
    Tags(TagSet),
    /// A filter [`push_filter`] writes as it is.
    Plain(Filter),
    /// This user did something to the post.
    ByUser(Link, i64),
    /// One of these posts (`similar:`, `search:`).
    Posts(Vec<i64>),
    /// In this pool, or with `None` in any pool that isn't deleted.
    Pool(Option<i32>),
    FavGroup(i32),
    /// In one of this user's favorite groups.
    OwnFavGroup(i64),
    /// The tagger suggests this tag, and the post doesn't have it yet.
    Suggested(i32),
    /// Pending, not uploaded by this viewer and not disapproved by them.
    Unmoderated(Option<i64>),
    /// Deleted with an open appeal.
    Appealed,
    /// The number of tags of this category.
    CategoryTags(i16, Bound<i64>),
    Not(Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
}

impl Node {
    fn not(self) -> Node {
        match self {
            Node::Const(value) => Node::Const(!value),
            Node::Not(inner) => *inner,
            node => Node::Not(Box::new(node)),
        }
    }

    /// Each of `nodes`, with constants folded.
    fn and(nodes: Vec<Node>) -> Node {
        let mut kept = Vec::with_capacity(nodes.len());
        for node in nodes {
            match node {
                Node::Const(true) => {}
                Node::Const(false) => return Node::Const(false),
                Node::And(inner) => kept.extend(inner),
                node => kept.push(node),
            }
        }
        match kept.len() {
            0 => Node::Const(true),
            1 => kept.pop().expect("one node"),
            _ => Node::And(kept),
        }
    }

    /// Any of `nodes`, with constants folded and tag sets merged.
    fn or(nodes: Vec<Node>) -> Node {
        let mut kept = Vec::with_capacity(nodes.len());
        let mut tags: Option<TagSet> = None;
        for node in nodes {
            match node {
                Node::Const(false) => {}
                Node::Const(true) => return Node::Const(true),
                Node::Or(inner) => kept.extend(inner),
                Node::Tags(set) => {
                    let union = tags.get_or_insert(TagSet {
                        ids: Vec::new(),
                        posts: 0,
                    });
                    union.ids.extend(set.ids);
                    union.posts += set.posts;
                }
                node => kept.push(node),
            }
        }
        if let Some(mut set) = tags {
            set.ids.sort_unstable();
            set.ids.dedup();
            kept.insert(0, Node::Tags(set));
        }
        match kept.len() {
            0 => Node::Const(false),
            1 => kept.pop().expect("one node"),
            _ => Node::Or(kept),
        }
    }

    /// Whether any filter in it needs `media_assets`.
    fn uses_media(&self) -> bool {
        match self {
            Node::Plain(filter) => matches!(
                filter,
                Filter::Width(_)
                    | Filter::Height(_)
                    | Filter::Mpixels(_)
                    | Filter::Ratio(_)
                    | Filter::FileSize(_)
                    | Filter::Duration(_)
                    | Filter::FileType(_)
                    | Filter::Md5(_)
                    | Filter::PixelHash(_)
                    | Filter::Exif { .. }
            ),
            Node::Not(inner) => inner.uses_media(),
            Node::And(nodes) | Node::Or(nodes) => nodes.iter().any(Node::uses_media),
            _ => false,
        }
    }

    /// Whether its SQL can be NULL rather than false (a video's missing
    /// duration, a post without uploader, …).
    fn nullable(&self) -> bool {
        match self {
            Node::Plain(_) | Node::ByUser(Link::Uploaded, _) | Node::ByUser(Link::Approved, _) => {
                true
            }
            Node::Not(inner) => inner.nullable(),
            Node::And(nodes) | Node::Or(nodes) => nodes.iter().any(Node::nullable),
            _ => false,
        }
    }

    fn push(&self, sql: &mut QueryBuilder<Postgres>) {
        match self {
            Node::Const(value) => {
                sql.push(if *value { "TRUE" } else { "FALSE" });
            }
            Node::Tags(set) => {
                sql.push("p.tag_ids && ")
                    .push_bind(set.ids.clone())
                    .push("::int4[]");
            }
            Node::Plain(filter) => {
                sql.push("(");
                push_filter(sql, filter);
                sql.push(")");
            }
            Node::ByUser(link, user) => {
                let (before, after) = match link {
                    Link::Uploaded => ("p.uploader_id = ", ""),
                    Link::Approved => ("p.approver_id = ", ""),
                    Link::Favorited => (
                        "EXISTS (SELECT 1 FROM favorites f WHERE f.post_id = p.id AND f.user_id = ",
                        ")",
                    ),
                    Link::Commented => (
                        "EXISTS (SELECT 1 FROM comments c WHERE c.post_id = p.id \
                         AND NOT c.is_deleted AND c.creator_id = ",
                        ")",
                    ),
                    Link::Noted => (
                        "EXISTS (SELECT 1 FROM note_versions nv WHERE nv.post_id = p.id \
                         AND nv.updater_id = ",
                        ")",
                    ),
                    Link::Flagged => (
                        "EXISTS (SELECT 1 FROM post_flags pf WHERE pf.post_id = p.id \
                         AND pf.creator_id = ",
                        ")",
                    ),
                    Link::Upvoted => (
                        "EXISTS (SELECT 1 FROM post_votes v WHERE v.post_id = p.id \
                         AND v.score = 1 AND v.user_id = ",
                        ")",
                    ),
                    Link::Downvoted => (
                        "EXISTS (SELECT 1 FROM post_votes v WHERE v.post_id = p.id \
                         AND v.score = -1 AND v.user_id = ",
                        ")",
                    ),
                };
                sql.push(before).push_bind(*user).push(after);
            }
            Node::Posts(ids) => {
                sql.push("p.id = ANY(").push_bind(ids.clone()).push(")");
            }
            Node::OwnFavGroup(user) => {
                sql.push(
                    "EXISTS (SELECT 1 FROM favorite_group_posts fg \
                     JOIN favorite_groups g ON g.id = fg.group_id \
                     WHERE fg.post_id = p.id AND g.creator_id = ",
                )
                .push_bind(*user)
                .push(")");
            }
            Node::Pool(Some(pool)) => {
                sql.push(
                    "EXISTS (SELECT 1 FROM pool_posts pp WHERE pp.post_id = p.id AND pp.pool_id = ",
                )
                .push_bind(*pool)
                .push(")");
            }
            Node::Pool(None) => {
                sql.push(
                    "EXISTS (SELECT 1 FROM pool_posts pp JOIN pools pl ON pl.id = pp.pool_id \
                     WHERE pp.post_id = p.id AND NOT pl.is_deleted)",
                );
            }
            Node::FavGroup(group) => {
                sql.push("EXISTS (SELECT 1 FROM favorite_group_posts fg WHERE fg.post_id = p.id AND fg.group_id = ")
                    .push_bind(*group)
                    .push(")");
            }
            Node::Suggested(tag) => {
                sql.push("(EXISTS (SELECT 1 FROM tag_suggestions ts WHERE ts.post_id = p.id AND ts.tag_id = ")
                    .push_bind(*tag)
                    .push(") AND NOT p.tag_ids @> ARRAY[")
                    .push_bind(*tag)
                    .push("]::int4[])");
            }
            Node::Unmoderated(viewer) => {
                sql.push("(p.status = 'pending'");
                if let Some(viewer) = viewer {
                    sql.push(" AND p.uploader_id IS DISTINCT FROM ")
                        .push_bind(*viewer)
                        .push(
                            " AND NOT EXISTS (SELECT 1 FROM post_disapprovals d \
                             WHERE d.post_id = p.id AND d.user_id = ",
                        )
                        .push_bind(*viewer)
                        .push(")");
                }
                sql.push(")");
            }
            Node::CategoryTags(category, count) => {
                push_bound(sql, &category_count(*category), count);
            }
            Node::Appealed => {
                sql.push("EXISTS (SELECT 1 FROM post_appeals x WHERE x.post_id = p.id AND x.status = 'open')");
            }
            // NOT would let NULLs through as unknown; IS NOT TRUE counts
            // them as not matching. Plain NOT keeps anti-joins possible.
            Node::Not(inner) if inner.nullable() => {
                sql.push("(");
                inner.push(sql);
                sql.push(") IS NOT TRUE");
            }
            Node::Not(inner) => {
                sql.push("NOT ");
                inner.push(sql);
            }
            Node::And(nodes) | Node::Or(nodes) => {
                let separator = if matches!(self, Node::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                sql.push("(");
                for (i, node) in nodes.iter().enumerate() {
                    if i > 0 {
                        sql.push(separator);
                    }
                    node.push(sql);
                }
                sql.push(")");
            }
        }
    }
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
    /// Filters and groups, each of which must match.
    filters: Vec<Node>,
    /// `ordfav:`'s user.
    ordfav: Option<i64>,
    /// `ordpool:`'s pool.
    ordpool: Option<i32>,
    /// `ordfavgroup:`'s group.
    ordfavgroup: Option<i32>,
    /// `order:<category>tags`'s category.
    ordcategory: Option<i16>,
    /// `order:custom`'s posts, in order: the search's `id:` list.
    custom: Vec<i64>,
    statuses: Vec<&'static str>,
    /// The viewer, if their own pending posts are included.
    own_pending: Option<i64>,
    /// The only ratings the viewer may see; empty for all.
    ratings: Vec<&'static str>,
    order: Order,
    per_page: u32,
    /// Estimated number of posts, for choosing a strategy.
    total: f64,
    max_page: u32,
    count_limit: u32,
    count_cost_limit: u32,
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
            filters: Vec::new(),
            ordfav: None,
            ordpool: None,
            ordfavgroup: None,
            ordcategory: None,
            custom: Vec::new(),
            statuses,
            own_pending,
            ratings: if visibility.ratings.is_empty() {
                Vec::new()
            } else {
                visibility.rating_codes()
            },
            order: query.order.unwrap_or_default(),
            per_page: query.limit.unwrap_or(config.per_page),
            total: 0.0,
            max_page: config.max_page,
            count_limit: config.count_limit,
            count_cost_limit: config.count_cost_limit,
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
        for condition in &query.conditions {
            // Already folded into `statuses`, unless it's a condition on
            // each post.
            if let Filter::Status(status) = condition.filter
                && !matches!(status, StatusFilter::Unmoderated | StatusFilter::Appealed)
            {
                continue;
            }
            let node = resolve_filter(db, &condition.filter, visibility, config).await?;
            plan.add(if condition.negated { node.not() } else { node });
        }
        for group in &query.groups {
            let node = resolve_expr(db, group, visibility, config).await?;
            plan.add(node);
        }
        for set in plan.required.iter_mut().chain(plan.any.as_mut()) {
            set.ids.sort_unstable();
            set.ids.dedup();
        }
        // Banned artists' posts, as if the search left them out.
        plan.excluded.extend(&visibility.hidden_tags);
        plan.excluded.sort_unstable();
        plan.excluded.dedup();

        if let Some(group) = &query.ordfavgroup
            && plan.order == Order::FavGroup
        {
            match favorite_group(db, group, visibility.viewer).await? {
                Some(id) => plan.ordfavgroup = Some(id),
                None => plan.nothing = true,
            }
        }
        if let Some(category) = &query.ordcategory
            && matches!(plan.order, Order::CategoryTagsDesc | Order::CategoryTagsAsc)
        {
            plan.ordcategory = Some(category_id(db, category).await?);
        }
        if let Some(pool) = &query.ordpool
            && plan.order == Order::Pool
        {
            match crate::pools::find(db, pool)
                .await?
                .filter(|p| !p.is_deleted)
            {
                Some(pool) => plan.ordpool = Some(pool.id),
                None => plan.nothing = true,
            }
        }

        if plan.order == Order::Custom {
            plan.custom = query
                .conditions
                .iter()
                .find_map(|condition| match (&condition.filter, condition.negated) {
                    (Filter::Id(Bound::In(ids)), false) => Some(ids.clone()),
                    (Filter::Id(Bound::Eq(id)), false) => Some(vec![*id]),
                    _ => None,
                })
                .ok_or_else(|| {
                    SearchError::Invalid(
                        "order:custom needs a list of posts, like id:3,1,2.".into(),
                    )
                })?;
        }

        if let Some(name) = &query.ordfav
            && plan.order == Order::Favorited
        {
            match crate::users::by_name_or_former(db, name).await? {
                Some(user) => plan.ordfav = Some(user.id),
                None => plan.nothing = true,
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

    /// Adds a condition every post must meet.
    fn add(&mut self, node: Node) {
        match node {
            Node::Const(true) => {}
            Node::Const(false) => self.nothing = true,
            // Tags on either side of an `or`: served like `~` tags.
            Node::Tags(set) => self.required.push(set),
            Node::And(nodes) => nodes.into_iter().for_each(|node| self.add(node)),
            node => self.filters.push(node),
        }
    }

    /// Leaves out posts any of `exclusions` matches, as a search
    /// `-(rule1) -(rule2) …` would. Rules of a single tag join the
    /// excluded tags, which cost one array check together.
    pub fn exclude(&mut self, exclusions: &[Exclusion]) {
        for exclusion in exclusions {
            if let ([tag], [], [], []) = (
                &exclusion.tags[..],
                &exclusion.not_tags[..],
                &exclusion.ratings[..],
                &exclusion.not_ratings[..],
            ) {
                self.excluded.push(*tag);
                continue;
            }
            let single = |id: &i32| {
                Node::Tags(TagSet {
                    ids: vec![*id],
                    posts: 0,
                })
            };
            let mut nodes: Vec<Node> = exclusion.tags.iter().map(single).collect();
            if !exclusion.not_tags.is_empty() {
                nodes.push(
                    Node::Tags(TagSet {
                        ids: exclusion.not_tags.clone(),
                        posts: 0,
                    })
                    .not(),
                );
            }
            nodes.extend(
                exclusion
                    .ratings
                    .iter()
                    .map(|ratings| Node::Plain(Filter::Rating(ratings.clone()))),
            );
            if !exclusion.not_ratings.is_empty() {
                nodes.push(Node::Plain(Filter::Rating(exclusion.not_ratings.clone())).not());
            }
            self.add(Node::and(nodes).not());
        }
        self.excluded.sort_unstable();
        self.excluded.dedup();
    }

    /// Posts per page.
    pub fn per_page(&self) -> u32 {
        self.per_page
    }

    pub fn order(&self) -> Order {
        self.order
    }

    /// Whether id order is served by the upload-date index instead: with a
    /// date filter, walking ids from the newest would skip every post newer
    /// than the range first. Posts get their upload time on insert, so the
    /// two orders are the same (up to posts uploaded in the same instant,
    /// which `id` still breaks).
    fn orders_by_date(&self) -> bool {
        matches!(self.order, Order::IdDesc | Order::IdAsc)
            && self.filters.iter().any(|node| {
                matches!(
                    node,
                    Node::Plain(Filter::Date { from: Some(_), .. })
                        | Node::Plain(Filter::Date { until: Some(_), .. })
                        | Node::Plain(Filter::Age(_))
                )
            })
    }

    /// What decides this search's results, as text: equal for searches that
    /// match the same posts for the same viewers, e.g. for caching counts.
    pub fn fingerprint(&self) -> String {
        format!(
            "{:?}",
            (
                self.nothing,
                (&self.statuses, &self.ratings),
                self.own_pending,
                self.required.iter().map(|s| &s.ids).collect::<Vec<_>>(),
                self.any.as_ref().map(|s| &s.ids),
                &self.excluded,
                &self.filters,
                (self.ordfav, self.ordpool, self.ordfavgroup),
            )
        )
    }

    /// Whether the order leaves out posts without comments.
    fn only_commented(&self) -> bool {
        matches!(self.order, Order::CommentDesc | Order::CommentAsc)
    }

    /// Whether the order leaves out older posts and those without a
    /// positive score.
    fn only_ranked(&self) -> bool {
        self.order == Order::Rank
    }

    /// Whether the order leaves out posts no comment bumped.
    fn only_bumped(&self) -> bool {
        matches!(
            self.order,
            Order::CommentBumpedDesc | Order::CommentBumpedAsc
        )
    }

    /// Whether the order leaves out posts without notes.
    fn only_noted(&self) -> bool {
        matches!(self.order, Order::NoteDesc | Order::NoteAsc)
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

    /// How [`Plan::ids`] gets `page`: `natural`, `walk` or `collect` (see
    /// the module docs).
    pub fn strategy_name(&self, page: PageRef) -> &'static str {
        let offset = match page {
            PageRef::Number(n) => n.saturating_sub(1).saturating_mul(self.per_page),
            _ => 0,
        };
        match self.strategy(offset) {
            Strategy::Natural => "natural",
            Strategy::Walk => "walk",
            Strategy::Collect => "collect",
        }
    }

    /// Runs the queries behind [`Plan::ids`] and [`Plan::count`] under
    /// `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` and returns their plans,
    /// `None` where no query would run.
    pub async fn explain(
        &self,
        db: &PgPool,
        page: PageRef,
    ) -> Result<(Option<Json>, Option<Json>), SearchError> {
        if self.nothing {
            return Ok((None, None));
        }
        const EXPLAIN: &str = "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) ";
        let mut ids = self.ids_query(page, EXPLAIN)?;
        let ids: Json = ids.build_query_scalar().fetch_one(db).await?;
        let count = match self.counted_query(db, EXPLAIN).await? {
            Some(mut query) => Some(query.build_query_scalar().fetch_one(db).await?),
            None => None,
        };
        Ok((Some(ids), count))
    }

    /// The same as [`Plan::explain`], as `EXPLAIN`'s text, for people.
    pub async fn explain_text(&self, db: &PgPool, page: PageRef) -> Result<String, SearchError> {
        if self.nothing {
            return Ok("(nothing to run: the search can't match)".into());
        }
        const EXPLAIN: &str = "EXPLAIN (ANALYZE, BUFFERS) ";
        let mut out = String::from("-- ids\n");
        let mut ids = self.ids_query(page, EXPLAIN)?;
        let lines: Vec<String> = ids.build_query_scalar().fetch_all(db).await?;
        out.push_str(&lines.join("\n"));
        match self.counted_query(db, EXPLAIN).await? {
            Some(mut count) => {
                let lines: Vec<String> = count.build_query_scalar().fetch_all(db).await?;
                out.push_str("\n-- count\n");
                out.push_str(&lines.join("\n"));
            }
            None => out.push_str("\n-- count: known or estimated, not counted"),
        }
        Ok(out)
    }

    /// The ids of the posts on `page`, in order.
    pub async fn ids(&self, db: &PgPool, page: PageRef) -> Result<Vec<i64>, SearchError> {
        let mut query = self.ids_query(page, "")?;
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

    /// The query for `page`'s ids, after `prefix` (for `EXPLAIN`).
    fn ids_query(
        &self,
        page: PageRef,
        prefix: &str,
    ) -> Result<QueryBuilder<Postgres>, SearchError> {
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
        let mut sql = QueryBuilder::new(format!("{prefix}SELECT p.id FROM posts p"));
        self.push_from_where(&mut sql, strategy);

        // Keyset pages walk away from the given id, so `a…` pages in
        // descending order (and `b…` pages in ascending order) are fetched
        // in reverse and flipped afterwards.
        let by_date = self.orders_by_date();
        // In date order, a cursor is the (upload time, id) of its post, or
        // of the nearest older one if it's gone.
        let cursor = |sql: &mut QueryBuilder<Postgres>, op: &str, id: i64| {
            if by_date {
                sql.push(format!(
                    " AND (p.created_at, p.id) {op} ((SELECT c.created_at FROM posts c WHERE c.id <= "
                ))
                .push_bind(id)
                .push(" ORDER BY c.id DESC LIMIT 1), ")
                .push_bind(id)
                .push(")");
            } else {
                sql.push(format!(" AND p.id {op} ")).push_bind(id);
            }
        };
        let descending = match (page, self.order) {
            (PageRef::Before(id), _) => {
                cursor(&mut sql, "<", id);
                true
            }
            (PageRef::After(id), _) => {
                cursor(&mut sql, ">", id);
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
            Order::IdDesc | Order::IdAsc if by_date => {
                let direction = if descending { "DESC" } else { "ASC" };
                let hide = if collect { " + interval '0 s'" } else { "" };
                sql.push(format!("p.created_at{hide} {direction}, p.id {direction}"));
            }
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
            Order::Favorited => {
                sql.push("fo.created_at DESC, p.id DESC");
            }
            Order::CommentDesc => {
                sql.push("p.last_commented_at DESC, p.id DESC");
            }
            Order::CommentAsc => {
                sql.push("p.last_commented_at ASC, p.id ASC");
            }
            Order::NoteDesc => {
                sql.push("p.last_noted_at DESC, p.id DESC");
            }
            Order::NoteAsc => {
                sql.push("p.last_noted_at ASC, p.id ASC");
            }
            Order::Pool => {
                sql.push("po.position ASC");
            }
            // Like Danbooru's: each tripling of the score is worth about
            // ten hours of age.
            Order::Rank => {
                sql.push(
                    "ln(p.score) / ln(3) + extract(epoch FROM p.created_at) / 35000 DESC, p.id DESC",
                );
            }
            Order::ChangeDesc => {
                sql.push("p.updated_at DESC, p.id DESC");
            }
            Order::ChangeAsc => {
                sql.push("p.updated_at ASC, p.id ASC");
            }
            Order::CategoryTagsDesc | Order::CategoryTagsAsc => {
                let direction = if self.order == Order::CategoryTagsDesc {
                    "DESC"
                } else {
                    "ASC"
                };
                let count = category_count(self.ordcategory.unwrap_or_default());
                sql.push(format!("{count} {direction}, p.id {direction}"));
            }
            Order::FavGroup => {
                sql.push("fgo.position ASC");
            }
            Order::UpvotesDesc => {
                sql.push("p.up_score DESC, p.id DESC");
            }
            Order::UpvotesAsc => {
                sql.push("p.up_score ASC, p.id ASC");
            }
            Order::DownvotesDesc => {
                sql.push("p.down_score DESC, p.id DESC");
            }
            Order::DownvotesAsc => {
                sql.push("p.down_score ASC, p.id ASC");
            }
            Order::CommentBumpedDesc => {
                sql.push("p.last_comment_bumped_at DESC, p.id DESC");
            }
            Order::CommentBumpedAsc => {
                sql.push("p.last_comment_bumped_at ASC, p.id ASC");
            }
            Order::CommentCountDesc => {
                sql.push("p.comment_count DESC, p.id DESC");
            }
            Order::CommentCountAsc => {
                sql.push("p.comment_count ASC, p.id ASC");
            }
            Order::NoteCountDesc => {
                sql.push("p.note_count DESC, p.id DESC");
            }
            Order::NoteCountAsc => {
                sql.push("p.note_count ASC, p.id ASC");
            }
            Order::Custom => {
                sql.push("array_position(")
                    .push_bind(self.custom.clone())
                    .push("::bigint[], p.id)");
            }
            Order::Md5Desc => {
                sql.push("a.md5 DESC, p.id DESC");
            }
            Order::Md5Asc => {
                sql.push("a.md5 ASC, p.id ASC");
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
                | Order::Md5Desc
                | Order::Md5Asc
        );
        media_order || self.filters.iter().any(Node::uses_media)
    }

    /// Everything after `SELECT … FROM posts p`: joins and the WHERE clause.
    fn push_from_where(&self, sql: &mut QueryBuilder<Postgres>, strategy: Strategy) {
        if self.needs_media() {
            sql.push(" JOIN media_assets a ON a.post_id = p.id");
        }
        if let Some(user) = self.ordfav {
            sql.push(" JOIN favorites fo ON fo.post_id = p.id AND fo.user_id = ")
                .push_bind(user);
        }
        if let Some(group) = self.ordfavgroup {
            sql.push(" JOIN favorite_group_posts fgo ON fgo.post_id = p.id AND fgo.group_id = ")
                .push_bind(group);
        }
        if let Some(pool) = self.ordpool {
            sql.push(" JOIN pool_posts po ON po.post_id = p.id AND po.pool_id = ")
                .push_bind(pool);
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
        if !self.ratings.is_empty() {
            sql.push(" AND p.rating = ANY(")
                .push_bind(self.ratings.clone())
                .push(")");
        }
        if self.only_commented() {
            sql.push(" AND p.last_commented_at IS NOT NULL");
        }
        if self.only_noted() {
            sql.push(" AND p.last_noted_at IS NOT NULL");
        }
        if self.only_bumped() {
            sql.push(" AND p.last_comment_bumped_at IS NOT NULL");
        }
        if self.only_ranked() {
            sql.push(format!(
                " AND p.score > 0 AND p.created_at > now() - interval '{RANK_DAYS} days'"
            ));
        }

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
        for node in &self.filters {
            sql.push(" AND ");
            node.push(sql);
        }
    }

    /// How many posts match. Exact up to the count limit; beyond it, a
    /// single tag's post count or the table size where those apply.
    pub async fn count(&self, db: &PgPool) -> Result<Count, SearchError> {
        if self.nothing {
            return Ok(Count::Exact(0));
        }
        if let Some(estimate) = self.count_shortcut() {
            return Ok(Count::About(estimate));
        }
        if let Some(estimate) = self.estimate_if_costly(db).await? {
            return Ok(Count::About(estimate));
        }
        let Some(mut sql) = self.count_query("") else {
            return Ok(Count::Exact(0));
        };
        let limit = i64::from(self.count_limit);
        let n: i64 = sql.build_query_scalar().fetch_one(db).await?;
        Ok(if n > limit {
            Count::AtLeast(limit)
        } else {
            Count::Exact(n)
        })
    }

    /// A count known without counting, when it's over the count limit
    /// anyway: the table size, or a lone tag's post count.
    fn count_shortcut(&self) -> Option<i64> {
        let limit = i64::from(self.count_limit);
        let unfiltered = self.filters.is_empty()
            && self.ordfav.is_none()
            && self.ordpool.is_none()
            && self.ordfavgroup.is_none()
            && !self.only_commented()
            && !self.only_noted()
            && !self.only_ranked()
            && self.ratings.is_empty()
            && self.excluded.is_empty()
            && self.any.is_none();
        let shortcut = match &self.required[..] {
            [] if unfiltered => Some(self.total as i64),
            [set] if unfiltered && set.ids.len() == 1 => Some(set.posts),
            _ => None,
        };
        shortcut.filter(|&n| n > limit)
    }

    /// The planner's estimate of the matches, when counting them would cost
    /// more than the count cost limit. Counting stops at the count limit,
    /// so only that share of the full cost counts.
    async fn estimate_if_costly(&self, db: &PgPool) -> Result<Option<i64>, SearchError> {
        let mut sql = QueryBuilder::new("EXPLAIN (FORMAT JSON) SELECT 1 FROM posts p");
        self.push_from_where(&mut sql, Strategy::Natural);
        let plan: Json = sql.build_query_scalar().fetch_one(db).await?;
        let root = &plan[0]["Plan"];
        let (Some(rows), Some(cost)) = (root["Plan Rows"].as_f64(), root["Total Cost"].as_f64())
        else {
            return Ok(None);
        };
        let counted = (f64::from(self.count_limit) + 1.0) / rows.max(1.0);
        let cost = cost * counted.min(1.0);
        Ok((cost > f64::from(self.count_cost_limit)).then(|| rows.round() as i64))
    }

    /// The counting query [`Plan::count`] would run, after `prefix`.
    async fn counted_query(
        &self,
        db: &PgPool,
        prefix: &str,
    ) -> Result<Option<QueryBuilder<Postgres>>, SearchError> {
        if self.nothing || self.count_shortcut().is_some() {
            return Ok(None);
        }
        if self.estimate_if_costly(db).await?.is_some() {
            return Ok(None);
        }
        Ok(self.count_query(prefix))
    }

    /// The counting query after `prefix`, unless the count needs none.
    fn count_query(&self, prefix: &str) -> Option<QueryBuilder<Postgres>> {
        if self.nothing || self.count_shortcut().is_some() {
            return None;
        }
        let limit = i64::from(self.count_limit);
        let mut sql = QueryBuilder::new(format!(
            "{prefix}SELECT count(*) FROM (SELECT 1 FROM posts p"
        ));
        self.push_from_where(&mut sql, Strategy::Natural);
        sql.push(" LIMIT ").push_bind(limit + 1).push(") AS hits");
        Some(sql)
    }
}

/// A group of a search, resolved.
fn resolve_expr<'a>(
    db: &'a PgPool,
    expr: &'a Expr,
    visibility: &'a Visibility,
    config: &'a SearchConfig,
) -> BoxFuture<'a, Result<Node, SearchError>> {
    Box::pin(async move {
        Ok(match expr {
            Expr::Tag(term) => {
                let set = expand(db, term, config.wildcard_limit).await?;
                if set.ids.is_empty() {
                    Node::Const(false)
                } else {
                    Node::Tags(set)
                }
            }
            Expr::Filter(filter) => resolve_filter(db, filter, visibility, config).await?,
            Expr::Not(inner) => resolve_expr(db, inner, visibility, config).await?.not(),
            Expr::And(items) | Expr::Or(items) => {
                let mut nodes = Vec::with_capacity(items.len());
                for item in items {
                    nodes.push(resolve_expr(db, item, visibility, config).await?);
                }
                if matches!(expr, Expr::And(_)) {
                    Node::and(nodes)
                } else {
                    Node::or(nodes)
                }
            }
        })
    })
}

/// A filter with the users, pools, groups and posts it names looked up;
/// what can't be found matches nothing.
async fn resolve_filter(
    db: &PgPool,
    filter: &Filter,
    visibility: &Visibility,
    config: &SearchConfig,
) -> Result<Node, SearchError> {
    let found = |id: Option<Node>| id.unwrap_or(Node::Const(false));
    Ok(match filter {
        Filter::Status(StatusFilter::Any) => Node::Const(true),
        Filter::Status(StatusFilter::Unmoderated) => Node::Unmoderated(visibility.viewer),
        Filter::Status(StatusFilter::Appealed) => Node::Appealed,
        Filter::User(name)
        | Filter::Fav(name)
        | Filter::Approver(UserMatch::Name(name))
        | Filter::Commenter(name)
        | Filter::Noter(name)
        | Filter::Flagger(name)
        | Filter::Upvote(name)
        | Filter::Downvote(name) => {
            let link = match filter {
                Filter::User(_) => Link::Uploaded,
                Filter::Fav(_) => Link::Favorited,
                Filter::Approver(_) => Link::Approved,
                Filter::Commenter(_) => Link::Commented,
                Filter::Noter(_) => Link::Noted,
                Filter::Flagger(_) => Link::Flagged,
                Filter::Upvote(_) => Link::Upvoted,
                _ => Link::Downvoted,
            };
            let user = crate::users::by_name_or_former(db, name)
                .await?
                .map(|user| user.id);
            // Flags and votes are private: for staff, and for the flaggers
            // and voters themselves.
            let private = matches!(link, Link::Flagged | Link::Upvoted | Link::Downvoted);
            let user = user.filter(|&user| {
                !private || visibility.reviews_posts() || visibility.viewer == Some(user)
            });
            found(user.map(|user| Node::ByUser(link, user)))
        }
        Filter::Similar(post) => {
            let hash = match crate::media::for_post(db, *post).await? {
                Some(asset) => asset.phash,
                None => None,
            };
            match hash {
                Some(hash) => Node::Posts(
                    crate::media::similar(
                        db,
                        hash as u64,
                        crate::media::SIMILAR_MAX_DISTANCE,
                        None,
                        SIMILAR_LIMIT,
                    )
                    .await?
                    .into_iter()
                    .map(|s| s.post_id)
                    .collect(),
                ),
                // Unknown or not yet processed: nothing to compare with.
                None => Node::Const(false),
            }
        }
        Filter::FavGroup(PoolFilter::Any) => found(visibility.viewer.map(Node::OwnFavGroup)),
        // Visitors have no groups, so every post is in none of them.
        Filter::FavGroup(PoolFilter::None) => visibility
            .viewer
            .map_or(Node::Const(true), |user| Node::OwnFavGroup(user).not()),
        Filter::FavGroup(PoolFilter::In(group)) => found(
            favorite_group(db, group, visibility.viewer)
                .await?
                .map(Node::FavGroup),
        ),
        Filter::Search(label) => {
            let ids = match visibility.viewer {
                Some(viewer) => saved_search_posts(db, viewer, label, visibility, config).await?,
                None => Vec::new(),
            };
            if ids.is_empty() {
                Node::Const(false)
            } else {
                Node::Posts(ids)
            }
        }
        Filter::Ai(name) => {
            let name = crate::tag_relations::aliases_of(db, &[name.as_str()])
                .await?
                .pop()
                .map_or_else(|| name.as_str().to_owned(), |(_, consequent)| consequent);
            found(
                crate::tags::by_name(db, &name)
                    .await?
                    .map(|tag| Node::Suggested(tag.id)),
            )
        }
        Filter::CategoryTags { category, count } => {
            Node::CategoryTags(category_id(db, category).await?, count.clone())
        }
        Filter::Pool(PoolFilter::Any) => Node::Pool(None),
        Filter::Pool(PoolFilter::None) => Node::Pool(None).not(),
        Filter::Pool(PoolFilter::In(pool)) => found(
            crate::pools::find(db, pool)
                .await?
                .filter(|p| !p.is_deleted)
                .map(|pool| Node::Pool(Some(pool.id))),
        ),
        filter => Node::Plain(filter.clone()),
    })
}

/// The id of the tag category `typed` names, by name or short name.
async fn category_id(db: &PgPool, typed: &str) -> Result<i16, SearchError> {
    let name = moekura_core::search::CATEGORY_SHORT_NAMES
        .iter()
        .find(|(short, _)| *short == typed)
        .map_or(typed, |(_, name)| name);
    crate::tags::categories(db)
        .await?
        .into_iter()
        .find(|c| c.name == name)
        .map(|c| c.id)
        .ok_or_else(|| SearchError::Invalid(format!("There's no tag category `{typed}`.")))
}

/// SQL for a post's number of tags in `category`.
fn category_count(category: i16) -> String {
    format!(
        "coalesce(p.category_counts[{}], 0)",
        i32::from(category) + 1
    )
}

/// The favorite group `group` names for `viewer`: by id, if it's public
/// or theirs; by name, one of theirs.
async fn favorite_group(
    db: &PgPool,
    group: &moekura_core::search::PoolRef,
    viewer: Option<i64>,
) -> sqlx::Result<Option<i32>> {
    use moekura_core::search::PoolRef;
    let found = match group {
        PoolRef::Id(id) => crate::favorite_groups::by_id(db, *id)
            .await?
            .filter(|g| g.is_public || Some(g.creator_id) == viewer),
        PoolRef::Name(name) => match viewer {
            Some(viewer) => crate::favorite_groups::by_name(db, viewer, name).await?,
            None => None,
        },
    };
    Ok(found.map(|g| g.id))
}

/// The newest posts matching `viewer`'s saved searches labelled `label`
/// (`all`: every one), up to [`SAVED_SEARCH_POSTS`] per search. Saved
/// searches that use `search:` themselves are skipped.
async fn saved_search_posts(
    db: &PgPool,
    viewer: i64,
    label: &str,
    visibility: &Visibility,
    config: &SearchConfig,
) -> Result<Vec<i64>, SearchError> {
    let label = (label != "all").then_some(label);
    let queries = crate::saved_searches::queries(db, viewer, label).await?;
    let config = SearchConfig {
        per_page: SAVED_SEARCH_POSTS,
        max_per_page: SAVED_SEARCH_POSTS,
        ..config.clone()
    };
    let runnable = queries
        .iter()
        .take(SAVED_SEARCH_LIMIT)
        .filter_map(|text| Query::parse(text).ok())
        .filter(|query| {
            !query
                .filters()
                .iter()
                .any(|filter| matches!(filter, Filter::Search(_)))
        })
        .map(|mut query| {
            // The newest posts, whatever order the search was saved with.
            query.order = None;
            query.ordfav = None;
            query.ordpool = None;
            query.ordfavgroup = None;
            query.limit = None;
            query
        })
        .collect::<Vec<_>>();
    let config = &config;
    let found: Vec<Vec<i64>> = stream::iter(runnable)
        .map(|query| async move {
            match Box::pin(Plan::resolve(db, &query, visibility, config)).await {
                Ok(plan) => plan.ids(db, PageRef::Number(1)).await,
                Err(SearchError::Invalid(_)) => Ok(Vec::new()),
                Err(e) => Err(e),
            }
        })
        .buffer_unordered(SAVED_SEARCH_CONCURRENCY)
        .try_collect()
        .await?;
    let mut ids = found.concat();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// The statuses a search covers, and the viewer if their own pending
/// posts are included. Without a `status:` filter, deleted posts are left
/// out even for those who may see them, unless they chose otherwise.
fn statuses(query: &Query, visibility: &Visibility) -> (Vec<&'static str>, Option<i64>) {
    let visible = &visibility.statuses;
    // `status:` inside a group decides for each post.
    let nested_status = query
        .groups
        .iter()
        .flat_map(|group| {
            let mut filters = Vec::new();
            group.filters(&mut filters);
            filters
        })
        .any(|filter| matches!(filter, Filter::Status(_)));
    let mut wanted: Vec<PostStatus> = match query.status() {
        None if nested_status || visibility.deleted_by_default => visible.clone(),
        None => visible
            .iter()
            .copied()
            .filter(|s| *s != PostStatus::Deleted)
            .collect(),
        Some(status) => match status.post_statuses() {
            Some(statuses) => visible
                .iter()
                .copied()
                .filter(|s| statuses.contains(s))
                .collect(),
            None => {
                // The rest is a condition on each post, of these statuses.
                let only = match status {
                    StatusFilter::Unmoderated => Some(PostStatus::Pending),
                    StatusFilter::Appealed => Some(PostStatus::Deleted),
                    _ => None,
                };
                visible
                    .iter()
                    .copied()
                    .filter(|s| only.is_none_or(|only| *s == only))
                    .collect()
            }
        },
    };
    let mut own_pending = matches!(
        query.status(),
        None | Some(StatusFilter::Any | StatusFilter::Pending | StatusFilter::Modqueue)
    );
    for condition in &query.conditions {
        if let (true, Filter::Status(status)) = (condition.negated, &condition.filter)
            && let Some(excluded) = status.post_statuses()
        {
            wanted.retain(|s| !excluded.contains(s));
            if excluded.contains(&PostStatus::Pending) {
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
        Filter::CommentCount(b) => push_bound(sql, "p.comment_count", b),
        Filter::NoteCount(b) => push_bound(sql, "p.note_count", b),
        Filter::Note(words) => {
            sql.push(
                "EXISTS (SELECT 1 FROM notes n WHERE n.post_id = p.id AND n.is_active \
                 AND to_tsvector('simple', n.body) @@ plainto_tsquery('simple', ",
            )
            .push_bind(words.clone())
            .push("))");
        }
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
        Filter::PixelHash(hashes) => {
            let hashes: Vec<Vec<u8>> = hashes.iter().map(|h| h.to_vec()).collect();
            sql.push("a.pixel_hash = ANY(").push_bind(hashes).push(")");
        }
        Filter::Date { from, until } => push_dates(sql, "p.created_at", *from, *until),
        Filter::Age(ago) => push_ago(sql, "p.created_at", ago),
        Filter::Updated(When::Ago(ago)) => push_ago(sql, "p.updated_at", ago),
        Filter::Updated(When::Dates { from, until }) => {
            push_dates(sql, "p.updated_at", *from, *until);
        }
        Filter::Child(any) => {
            if !any {
                sql.push("NOT ");
            }
            sql.push(
                "EXISTS (SELECT 1 FROM posts c WHERE c.parent_id = p.id AND c.status <> 'deleted')",
            );
        }
        Filter::Approver(UserMatch::Any) => {
            sql.push("p.approver_id IS NOT NULL");
        }
        Filter::Approver(UserMatch::None) => {
            sql.push("p.approver_id IS NULL");
        }
        Filter::Comment(words) => {
            sql.push(
                "EXISTS (SELECT 1 FROM comments c WHERE c.post_id = p.id AND NOT c.is_deleted \
                 AND to_tsvector('simple', c.body) @@ plainto_tsquery('simple', ",
            )
            .push_bind(words.clone())
            .push("))");
        }
        Filter::Exif { key, value } => {
            // metadata_search (migration 0062) is indexed.
            match value {
                Some(value) => {
                    sql.push("metadata_search(a.metadata) @> jsonb_build_object(")
                        .push_bind(key.clone())
                        .push("::text, ")
                        .push_bind(value.clone())
                        .push("::text)");
                }
                None => {
                    sql.push("metadata_search(a.metadata) ? ")
                        .push_bind(key.clone());
                }
            }
        }
        Filter::Embedded(on) => {
            if *on {
                sql.push("p.has_embedded_notes AND p.note_count > 0");
            } else {
                sql.push("NOT (p.has_embedded_notes AND p.note_count > 0)");
            }
        }
        Filter::Pixiv(PixivFilter::Any) => {
            sql.push("p.pixiv_id IS NOT NULL");
        }
        Filter::Pixiv(PixivFilter::None) => {
            sql.push("p.pixiv_id IS NULL");
        }
        Filter::Pixiv(PixivFilter::Id(b)) => {
            sql.push("p.pixiv_id IS NOT NULL AND ");
            push_bound(sql, "p.pixiv_id", b);
        }
        Filter::Commentary(c) => {
            if *c == CommentaryFilter::None {
                sql.push("NOT ");
            }
            sql.push("EXISTS (SELECT 1 FROM artist_commentaries ac WHERE ac.post_id = p.id");
            match c {
                CommentaryFilter::Any | CommentaryFilter::None => {}
                CommentaryFilter::Translated => {
                    sql.push(" AND (ac.translated_title <> '' OR ac.translated_description <> '')");
                }
                CommentaryFilter::Untranslated => {
                    sql.push(
                        " AND ac.translated_title = '' AND ac.translated_description = '' \
                         AND (ac.original_title <> '' OR ac.original_description <> '')",
                    );
                }
                CommentaryFilter::Words(words) => {
                    sql.push(
                        " AND to_tsvector('simple', ac.original_title || ' ' || \
                         ac.original_description || ' ' || ac.translated_title || ' ' || \
                         ac.translated_description) @@ plainto_tsquery('simple', ",
                    )
                    .push_bind(words.clone())
                    .push(")");
                }
            }
            sql.push(")");
        }
        Filter::Source(SourceFilter::Any) => {
            sql.push("p.source <> ''");
        }
        Filter::Source(SourceFilter::None) => {
            sql.push("p.source = ''");
        }
        // Posts without a source are left out of the index.
        Filter::Source(SourceFilter::Pattern(pattern)) => {
            sql.push("p.source <> '' AND p.source ILIKE ")
                .push_bind(crate::tags::like_pattern(pattern));
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
        // Inside groups; at the top level, statuses are folded into the
        // statuses searched.
        Filter::Status(status) if status.post_statuses().is_some() => {
            let statuses: Vec<&str> = status
                .post_statuses()
                .unwrap_or_default()
                .iter()
                .map(|s| s.as_str())
                .collect();
            sql.push("p.status = ANY(").push_bind(statuses).push(")");
        }
        Filter::Status(_)
        | Filter::User(_)
        | Filter::Fav(_)
        | Filter::Approver(UserMatch::Name(_))
        | Filter::Commenter(_)
        | Filter::Noter(_)
        | Filter::Flagger(_)
        | Filter::Upvote(_)
        | Filter::Downvote(_)
        | Filter::CategoryTags { .. }
        | Filter::Similar(_)
        | Filter::Search(_)
        | Filter::FavGroup(_)
        | Filter::Ai(_)
        | Filter::Pool(_) => {
            unreachable!("resolved in Plan::resolve")
        }
    }
}

/// `column` on or after `from` and before `until` (UTC days).
fn push_dates(
    sql: &mut QueryBuilder<Postgres>,
    column: &str,
    from: Option<time::Date>,
    until: Option<time::Date>,
) {
    let midnight = |d: time::Date| d.midnight().assume_utc();
    sql.push("TRUE");
    if let Some(from) = from {
        sql.push(format!(" AND {column} >= "))
            .push_bind(midnight(from));
    }
    if let Some(until) = until {
        sql.push(format!(" AND {column} < "))
            .push_bind(midnight(until));
    }
}

/// `column` (a time) `ago` back from now: `<1w` is newer than a week ago.
fn push_ago(sql: &mut QueryBuilder<Postgres>, column: &str, ago: &Bound<Age>) {
    let compare = |sql: &mut QueryBuilder<Postgres>, op: &str, age: &Age| {
        sql.push(format!("{column} {op} now() - make_interval(secs => "))
            .push_bind(age.seconds() as f64)
            .push(")");
    };
    match ago {
        Bound::Lt(age) => compare(sql, ">", age),
        // `age:1d`: within a day.
        Bound::Le(age) | Bound::Eq(age) => compare(sql, ">=", age),
        Bound::Gt(age) => compare(sql, "<", age),
        Bound::Ge(age) => compare(sql, "<=", age),
        Bound::Between(newest, oldest) => {
            compare(sql, ">=", oldest);
            sql.push(" AND ");
            compare(sql, "<=", newest);
        }
        // The parser doesn't make these.
        Bound::In(_) => {
            sql.push("FALSE");
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
    use moekura_core::posts::PostStatus;
    use serde_json::Value;
    use sqlx::{Execute, PgPool};

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
            hidden_tags: Vec::new(),
            statuses: vec![PostStatus::Active, PostStatus::Flagged],
            viewer: None,
            ratings: Vec::new(),
            deleted_by_default: false,
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
    async fn exclusions(pool: PgPool) {
        let post = |tags, rating| Seed {
            tags,
            rating,
            ..Seed::default()
        };
        let spider = seed(&pool, post(&["spider", "cute"], "g")).await;
        let gore = seed(&pool, post(&["gore"], "g")).await;
        let mild = seed(&pool, post(&["gore", "safe_version"], "g")).await;
        let cat = seed(&pool, post(&["cat"], "e")).await;
        let tame = seed(&pool, post(&["cat"], "s")).await;
        let plain = seed(&pool, post(&[], "q")).await;
        let id = |name: &str| {
            let pool = pool.clone();
            let name = name.to_owned();
            async move {
                crate::tags::by_names(&pool, &[name.as_str()])
                    .await
                    .unwrap()[0]
                    .id
            }
        };
        let rules = [
            // `spider`
            Exclusion {
                tags: vec![id("spider").await],
                ..Exclusion::default()
            },
            // `gore -safe_version`
            Exclusion {
                tags: vec![id("gore").await],
                not_tags: vec![id("safe_version").await],
                ..Exclusion::default()
            },
            // `cat rating:e,q`
            Exclusion {
                tags: vec![id("cat").await],
                ratings: vec![vec![Rating::Explicit, Rating::Questionable]],
                ..Exclusion::default()
            },
            // `-rating:g,s,e`: everything questionable
            Exclusion {
                not_ratings: vec![Rating::General, Rating::Sensitive, Rating::Explicit],
                ..Exclusion::default()
            },
        ];
        let excluding = async |input: &str, rules: &[Exclusion]| {
            let query = Query::parse(input).unwrap();
            let mut plan = Plan::resolve(&pool, &query, &public(), &SearchConfig::default())
                .await
                .unwrap();
            plan.exclude(rules);
            let ids = plan.ids(&pool, PageRef::default()).await.unwrap();
            (ids, plan.count(&pool).await.unwrap())
        };
        assert_eq!(
            search(&pool, "").await,
            [plain, tame, cat, mild, gore, spider]
        );
        assert_eq!(
            excluding("", &rules).await,
            (vec![tame, mild], Count::Exact(2))
        );
        assert_eq!(excluding("cat", &rules).await.0, [tame]);
        assert_eq!(excluding("cute", &rules).await.0, Vec::<i64>::new());
        // A rule without terms matches every post.
        assert_eq!(
            excluding("", &[Exclusion::default()]).await,
            (Vec::new(), Count::Exact(0))
        );
        assert_eq!(excluding("", &[]).await.1, Count::Exact(6));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn groups(pool: PgPool) {
        let alice: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let cat = seed(
            &pool,
            Seed {
                tags: &["cat", "cute"],
                rating: "e",
                ..Seed::default()
            },
        )
        .await;
        let dog = seed(
            &pool,
            Seed {
                tags: &["dog", "cute"],
                uploader: Some(alice),
                ..Seed::default()
            },
        )
        .await;
        let both = seed(
            &pool,
            Seed {
                tags: &["cat", "dog"],
                score: 5,
                ..Seed::default()
            },
        )
        .await;
        let clip = seed(
            &pool,
            Seed {
                media_type: "webm",
                duration_ms: Some(5000),
                ..Seed::default()
            },
        )
        .await;
        let deleted = seed(
            &pool,
            Seed {
                tags: &["cat"],
                status: "deleted",
                ..Seed::default()
            },
        )
        .await;

        let cases: &[(&str, &[i64])] = &[
            ("(cat or dog) -rating:e", &[both, dog]),
            ("(cat dog) or (cute rating:e)", &[both, cat]),
            ("-(cat dog)", &[clip, dog, cat]),
            ("-(cat or dog)", &[clip]),
            ("cat or user:alice", &[both, dog, cat]),
            ("cat or user:nobody", &[both, cat]),
            ("-(dog user:nobody)", &[clip, both, dog, cat]),
            ("dog (user:alice or score:>0)", &[both, dog]),
            ("(missing or cute) -dog", &[cat]),
            ("missing or nothing_*", &[]),
            ("(filetype:webm or rating:e)", &[clip, cat]),
            ("-(duration:>1 or cat)", &[dog]),
            ("(cat or dog) (cute or score:5)", &[both, dog, cat]),
            // status: inside a group decides for each post.
            ("cat (status:deleted or rating:e)", &[cat]),
        ];
        for (input, expected) in cases {
            assert_eq!(&search(&pool, input).await, expected, "{input}");
        }
        let staff = Visibility {
            hidden_tags: Vec::new(),
            statuses: vec![PostStatus::Active, PostStatus::Deleted],
            viewer: None,
            ratings: Vec::new(),
            deleted_by_default: false,
        };
        assert_eq!(
            search_as(&pool, "cat (status:deleted or rating:e)", &staff).await,
            [deleted, cat]
        );

        // Counting agrees.
        let query = Query::parse("(cat dog) or (cute rating:e)").unwrap();
        let plan = Plan::resolve(&pool, &query, &public(), &SearchConfig::default())
            .await
            .unwrap();
        assert_eq!(plan.count(&pool).await.unwrap(), Count::Exact(2));
        // Nested terms count towards the limit.
        let config = SearchConfig {
            max_terms: 3,
            ..SearchConfig::default()
        };
        let query = Query::parse("a (b or (c d))").unwrap();
        assert!(matches!(
            Plan::resolve(&pool, &query, &public(), &config).await,
            Err(SearchError::Invalid(_))
        ));
        // Or-ed tags are a tag set, as with `~`.
        let query = Query::parse("(cat or dog) (cute or missing)").unwrap();
        let plan = Plan::resolve(&pool, &query, &public(), &SearchConfig::default())
            .await
            .unwrap();
        assert_eq!(plan.required.len(), 1);
        assert_eq!(plan.any.as_ref().map(|set| set.ids.len()), Some(2));
        assert!(plan.filters.is_empty());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn ages_and_changes(pool: PgPool) {
        let mut ids = Vec::new();
        for days_ago in [40, 10, 3, 0] {
            ids.push(
                seed(
                    &pool,
                    Seed {
                        days_ago,
                        ..Seed::default()
                    },
                )
                .await,
            );
        }
        // Changed: the oldest a day ago, the newest ten days ago.
        for (post, days) in [(ids[0], 1), (ids[3], 10)] {
            sqlx::query(
                "UPDATE posts SET updated_at = now() - make_interval(days => $2) WHERE id = $1",
            )
            .bind(post)
            .bind(days)
            .execute(&pool)
            .await
            .unwrap();
        }
        let by = |order: &[usize]| order.iter().map(|&i| ids[i]).collect::<Vec<_>>();
        let cases: &[(&str, Vec<i64>)] = &[
            ("age:<1w", by(&[3, 2])),
            ("age:1d", by(&[3])),
            ("age:>1w", by(&[1, 0])),
            ("age:2d..2w", by(&[2, 1])),
            ("age:1mo..", by(&[0])),
            ("-age:<1w", by(&[1, 0])),
            ("updated:<2d", by(&[2, 1, 0])),
            ("updated:>1w", by(&[3])),
            // Further back than times go: every post is newer.
            ("age:<9223372036854775807y", by(&[3, 2, 1, 0])),
            ("updated:>9223372036854775807s", by(&[])),
            ("order:change", by(&[2, 1, 0, 3])),
            ("order:change_asc", by(&[3, 0, 1, 2])),
        ];
        for (input, expected) in cases {
            assert_eq!(&search(&pool, input).await, expected, "{input}");
        }
        let today = time::OffsetDateTime::now_utc().date();
        assert_eq!(
            search(&pool, &format!("updated:{today}")).await,
            by(&[2, 1])
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn hot_posts(pool: PgPool) {
        let post = |score, hours_ago: i32| {
            let pool = pool.clone();
            async move {
                let id = seed(
                    &pool,
                    Seed {
                        score,
                        ..Seed::default()
                    },
                )
                .await;
                sqlx::query(
                    "UPDATE posts SET created_at = now() - make_interval(hours => $2) WHERE id = $1",
                )
                .bind(id)
                .bind(hours_ago)
                .execute(&pool)
                .await
                .unwrap();
                id
            }
        };
        let new_ok = post(3, 1).await;
        let old_great = post(30, 20).await;
        let new_great = post(27, 2).await;
        let zero = post(0, 1).await;
        let too_old = post(100, 60).await;
        // 27 is three triplings of 1, worth about 29 hours: two hours old
        // beats 20 hours old at slightly more.
        assert_eq!(
            search(&pool, "order:rank").await,
            [new_great, old_great, new_ok]
        );
        assert!(!search(&pool, "order:rank").await.contains(&zero));
        assert!(!search(&pool, "order:rank").await.contains(&too_old));
        let query = Query::parse("order:rank").unwrap();
        let plan = Plan::resolve(&pool, &query, &public(), &SearchConfig::default())
            .await
            .unwrap();
        assert_eq!(plan.count(&pool).await.unwrap(), Count::Exact(3));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn category_tags(pool: PgPool) {
        let a = seed(
            &pool,
            Seed {
                tags: &["alice_(artist)", "bob_(artist)", "cat"],
                ..Seed::default()
            },
        )
        .await;
        let b = seed(
            &pool,
            Seed {
                tags: &["bob_(artist)", "cat", "dog", "tree"],
                ..Seed::default()
            },
        )
        .await;
        let c = seed(&pool, Seed::default()).await;
        // Moving tags to the artist category recounts their posts.
        for name in ["alice_(artist)", "bob_(artist)"] {
            let tag = crate::tags::by_name(&pool, name).await.unwrap().unwrap();
            crate::tags::update(&pool, tag.id, 1, false, None)
                .await
                .unwrap();
        }
        let cases: &[(&str, &[i64])] = &[
            ("arttags:0", &[c]),
            ("arttags:2", &[a]),
            ("artisttags:>0", &[b, a]),
            ("gentags:3", &[b]),
            ("generaltags:1 arttags:2", &[a]),
            ("-arttags:0 copytags:0", &[b, a]),
            ("(arttags:1 or gentags:0)", &[c, b]),
            ("order:gentags", &[b, a, c]),
            ("order:arttags_asc", &[c, b, a]),
        ];
        for (input, expected) in cases {
            assert_eq!(&search(&pool, input).await, expected, "{input}");
        }
        // Tags added later count too.
        let mut conn = pool.acquire().await.unwrap();
        let wanted = [WantedTag {
            name: "carol",
            category_id: Some(1),
        }];
        let carol = crate::tags::ensure(&mut conn, &wanted, false)
            .await
            .unwrap()[0]
            .id;
        drop(conn);
        sqlx::query("UPDATE posts SET tag_ids = ARRAY[$1] WHERE id = $2")
            .bind(carol)
            .bind(c)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(search(&pool, "arttags:1").await, [c, b]);

        let query = Query::parse("fishtags:0").unwrap();
        assert!(matches!(
            Plan::resolve(&pool, &query, &public(), &SearchConfig::default()).await,
            Err(SearchError::Invalid(message)) if message.contains("no tag category `fish`")
        ));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn people(pool: PgPool) {
        let user = |name: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
                )
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let (alice, bob) = (user("alice").await, user("bob").await);
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(seed(&pool, Seed::default()).await);
        }
        crate::posts::set_approver(&pool, ids[0], Some(alice))
            .await
            .unwrap();
        crate::comments::create(&pool, ids[0], bob, "Nice art, love the colours", true)
            .await
            .unwrap();
        let hidden = crate::comments::create(&pool, ids[1], bob, "nice try", true)
            .await
            .unwrap();
        crate::comments::set_deleted(&pool, hidden, true)
            .await
            .unwrap();
        let note_box = moekura_core::notes::NoteBox {
            x: 0,
            y: 0,
            width: 5,
            height: 5,
        };
        crate::notes::create(&pool, ids[1], note_box, "hi", Some(alice))
            .await
            .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        crate::flags::create(&mut conn, ids[2], bob, "off-topic")
            .await
            .unwrap();
        drop(conn);

        let cases: &[(&str, &[i64])] = &[
            ("approver:alice", &[ids[0]]),
            ("approver:any", &[ids[0]]),
            ("approver:none", &[ids[2], ids[1]]),
            ("-approver:alice", &[ids[2], ids[1]]),
            ("approver:nobody", &[]),
            ("commenter:bob", &[ids[0]]),
            ("comment:nice", &[ids[0]]),
            ("comment:love_the_colours", &[ids[0]]),
            ("-comment:nice", &[ids[2], ids[1]]),
            ("noter:alice", &[ids[1]]),
            ("noter:bob", &[]),
            // Flaggers are hidden from visitors.
            ("flagger:bob", &[]),
        ];
        for (input, expected) in cases {
            assert_eq!(&search(&pool, input).await, expected, "{input}");
        }
        let as_user = |viewer| Visibility {
            viewer: Some(viewer),
            ..public()
        };
        // The flagger sees their own flags; others don't.
        assert_eq!(
            search_as(&pool, "flagger:bob", &as_user(bob)).await,
            [ids[2]]
        );
        assert!(
            search_as(&pool, "flagger:bob", &as_user(alice))
                .await
                .is_empty()
        );
        let staff = Visibility {
            hidden_tags: Vec::new(),
            statuses: vec![PostStatus::Active, PostStatus::Flagged, PostStatus::Pending],
            viewer: Some(alice),
            ratings: Vec::new(),
            deleted_by_default: false,
        };
        assert_eq!(search_as(&pool, "flagger:bob", &staff).await, [ids[2]]);

        // Votes are private the same way.
        for (post, score) in [(ids[0], 1), (ids[1], -1)] {
            sqlx::query("INSERT INTO post_votes (user_id, post_id, score) VALUES ($1, $2, $3)")
                .bind(bob)
                .bind(post)
                .bind(score as i16)
                .execute(&pool)
                .await
                .unwrap();
        }
        assert_eq!(
            search_as(&pool, "upvote:bob", &as_user(bob)).await,
            [ids[0]]
        );
        assert_eq!(
            search_as(&pool, "downvote:bob", &as_user(bob)).await,
            [ids[1]]
        );
        assert_eq!(
            search_as(&pool, "-upvote:bob", &as_user(bob)).await,
            [ids[2], ids[1]]
        );
        assert_eq!(search_as(&pool, "upvote:bob", &staff).await, [ids[0]]);
        assert!(
            search_as(&pool, "upvote:bob", &as_user(alice))
                .await
                .is_empty()
        );
        assert!(search(&pool, "downvote:bob").await.is_empty());
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

        for (post, source) in [
            (clip, "https://Example.com/1"),
            (wide, "https://example.org/a?from=example.com"),
        ] {
            sqlx::query("UPDATE posts SET source = $2 WHERE id = $1")
                .bind(post)
                .bind(source)
                .execute(&pool)
                .await
                .unwrap();
        }
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
            ("child:any", &[wide]),
            ("child:none", &[clip, tall]),
            ("is:parent -is:sfw", &[wide]),
            ("has:source", &[clip, wide]),
            ("-has:source", &[tall]),
            ("source:none", &[tall]),
            ("source:https://example.com/", &[clip]),
            ("source:HTTPS://EXAMPLE.COM/", &[clip]),
            ("source:*example.com*", &[clip, wide]),
            ("-source:*example.com*", &[tall]),
            ("source:*.org/*", &[wide]),
            ("source:example", &[]),
            ("source:https://example.com/1_", &[]),
            // Date searches follow upload time, which is id order for
            // posts uploaded normally; `tall` is backdated here.
            ("date:>=2000", &[clip, wide, tall]),
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
        let pixels = [7u8; 16];
        crate::media::set_pixel_hash(
            &pool,
            sqlx::query_scalar("SELECT id FROM media_assets WHERE post_id = $1")
                .bind(clip)
                .fetch_one(&pool)
                .await
                .unwrap(),
            Some(&pixels),
        )
        .await
        .unwrap();
        assert_eq!(
            search(&pool, &format!("pixelhash:{}", hex::encode(pixels))).await,
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
    async fn favorites(pool: PgPool) {
        let alice: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let a = seed(
            &pool,
            Seed {
                tags: &["x"],
                ..Seed::default()
            },
        )
        .await;
        let b = seed(
            &pool,
            Seed {
                tags: &["x"],
                ..Seed::default()
            },
        )
        .await;
        let c = seed(&pool, Seed::default()).await;
        // Favorited in the order b, then a.
        for post in [b, a] {
            crate::favorites::add(&pool, alice, post).await.unwrap();
        }
        sqlx::query(
            "UPDATE favorites SET created_at = now() - make_interval(secs => post_id::int)",
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(search(&pool, "fav:alice").await, [b, a]);
        assert_eq!(search(&pool, "-fav:alice").await, [c]);
        assert_eq!(search(&pool, "x -fav:nobody").await, [b, a]);
        assert!(search(&pool, "fav:nobody").await.is_empty());
        // Newest favorite first: a's was made (a second) after b's.
        assert_eq!(search(&pool, "ordfav:alice").await, [a, b]);
        assert!(search(&pool, "ordfav:nobody").await.is_empty());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn similar_posts(pool: PgPool) {
        let a = seed(&pool, Seed::default()).await;
        let b = seed(&pool, Seed::default()).await;
        let far = seed(&pool, Seed::default()).await;
        let unprocessed = seed(&pool, Seed::default()).await;
        let base: u64 = 0xF0F0_1234_5678_9ABC;
        for (post, hash) in [(a, base), (b, base ^ 0b101), (far, !base)] {
            let asset: i64 = sqlx::query_scalar("SELECT id FROM media_assets WHERE post_id = $1")
                .bind(post)
                .fetch_one(&pool)
                .await
                .unwrap();
            crate::media::mark_processed(&pool, asset, Some(hash))
                .await
                .unwrap();
        }
        assert_eq!(search(&pool, &format!("similar:{a}")).await, [b, a]);
        assert_eq!(
            search(&pool, &format!("-similar:{a}")).await,
            [unprocessed, far]
        );
        assert!(
            search(&pool, &format!("similar:{unprocessed}"))
                .await
                .is_empty()
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
            hidden_tags: Vec::new(),
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
            hidden_tags: Vec::new(),
            statuses: vec![
                PostStatus::Active,
                PostStatus::Flagged,
                PostStatus::Pending,
                PostStatus::Deleted,
            ],
            viewer: None,
            ratings: Vec::new(),
            deleted_by_default: false,
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
        // The mod queue: pending or flagged.
        assert_eq!(
            search_as(&pool, "status:modqueue", &staff).await,
            [mine, pending, flagged]
        );
        assert_eq!(
            search_as(&pool, "status:modqueue", &member).await,
            [mine, flagged]
        );
        assert_eq!(search_as(&pool, "-status:modqueue", &staff).await, [active]);
        assert_eq!(
            search_as(&pool, "(status:modqueue or status:deleted)", &staff).await,
            [deleted, mine, pending, flagged]
        );

        // What's left for an approver: pending posts they didn't upload
        // or disapprove.
        let approver = Visibility {
            hidden_tags: Vec::new(),
            viewer: Some(viewer),
            ..staff.clone()
        };
        let another = seed(
            &pool,
            Seed {
                status: "pending",
                ..Seed::default()
            },
        )
        .await;
        assert_eq!(
            search_as(&pool, "status:unmoderated", &approver).await,
            [another, pending]
        );
        crate::disapprovals::disapprove(
            &pool,
            another,
            viewer,
            moekura_core::moderation::DisapprovalReason::Disinterest,
            "",
        )
        .await
        .unwrap();
        assert_eq!(
            search_as(&pool, "status:unmoderated", &approver).await,
            [pending]
        );
        assert_eq!(
            search_as(&pool, "status:pending -status:unmoderated", &approver).await,
            [another, mine]
        );
        assert!(
            search_as(&pool, "status:unmoderated", &member)
                .await
                .is_empty()
        );
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn comment_counts_and_order(pool: PgPool) {
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(
                seed(
                    &pool,
                    Seed {
                        tags: &["x"],
                        ..Seed::default()
                    },
                )
                .await,
            );
        }
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        // Post 0 gets two comments, then post 2 one.
        for post in [ids[0], ids[0], ids[2]] {
            crate::comments::create(&pool, post, user, "hi", true)
                .await
                .unwrap();
        }
        assert_eq!(search(&pool, "commentcount:2").await, [ids[0]]);
        assert_eq!(search(&pool, "commentcount:>0").await, [ids[2], ids[0]]);
        assert_eq!(search(&pool, "-commentcount:>0").await, [ids[1]]);
        assert_eq!(search(&pool, "order:comment").await, [ids[2], ids[0]]);
        assert_eq!(search(&pool, "x order:comment_asc").await, [ids[0], ids[2]]);

        let query = Query::parse("order:comment").unwrap();
        let plan = Plan::resolve(&pool, &query, &public(), &SearchConfig::default())
            .await
            .unwrap();
        assert_eq!(plan.count(&pool).await.unwrap(), Count::Exact(2));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn vote_comment_note_custom_and_md5_orders(pool: PgPool) {
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(
                seed(
                    &pool,
                    Seed {
                        tags: &["x"],
                        ..Seed::default()
                    },
                )
                .await,
            );
        }
        let mut users = Vec::new();
        for name in ["ann", "bob", "cat"] {
            users.push(
                sqlx::query_scalar::<_, i64>(
                    "INSERT INTO users (name, role_id)
                     SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
                )
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap(),
            );
        }
        // Post 0: two up, one down. Post 1: one up. Post 2: two down.
        for (user, post, score) in [
            (0, 0, 1),
            (1, 0, 1),
            (2, 0, -1),
            (0, 1, 1),
            (0, 2, -1),
            (1, 2, -1),
        ] {
            sqlx::query("INSERT INTO post_votes (user_id, post_id, score) VALUES ($1, $2, $3)")
                .bind(users[user])
                .bind(ids[post])
                .bind(score as i16)
                .execute(&pool)
                .await
                .unwrap();
        }
        // A changed vote moves between the counts.
        sqlx::query("UPDATE post_votes SET score = 1 WHERE user_id = $1 AND post_id = $2")
            .bind(users[1])
            .bind(ids[2])
            .execute(&pool)
            .await
            .unwrap();
        let by = |order: &[usize]| order.iter().map(|&i| ids[i]).collect::<Vec<_>>();
        assert_eq!(search(&pool, "order:upvotes").await, by(&[0, 2, 1]));
        assert_eq!(search(&pool, "order:upvotes_asc").await, by(&[1, 2, 0]));
        assert_eq!(search(&pool, "order:downvotes").await, by(&[2, 0, 1]));

        // Post 1 is commented on last, but without bumping.
        for (post, bump) in [(0, true), (2, true), (2, true), (1, false)] {
            crate::comments::create(&pool, ids[post], users[0], "hi", bump)
                .await
                .unwrap();
        }
        assert_eq!(search(&pool, "order:comment").await, by(&[1, 2, 0]));
        assert_eq!(search(&pool, "order:comment_bumped").await, by(&[2, 0]));
        assert_eq!(search(&pool, "order:comment_bumped_asc").await, by(&[0, 2]));
        assert_eq!(search(&pool, "order:comment_count").await, by(&[2, 1, 0]));

        sqlx::query("UPDATE posts SET note_count = 3 WHERE id = $1")
            .bind(ids[1])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(search(&pool, "order:note_count").await, by(&[1, 2, 0]));
        assert_eq!(search(&pool, "order:note_count_asc").await, by(&[0, 2, 1]));

        let custom = format!("id:{},{},{} order:custom", ids[1], ids[2], ids[0]);
        assert_eq!(search(&pool, &custom).await, by(&[1, 2, 0]));
        let query = Query::parse("x order:custom").unwrap();
        assert!(matches!(
            Plan::resolve(&pool, &query, &public(), &SearchConfig::default()).await,
            Err(SearchError::Invalid(_))
        ));

        let by_md5: Vec<i64> =
            sqlx::query_scalar("SELECT post_id FROM media_assets ORDER BY md5 DESC")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(search(&pool, "order:md5").await, by_md5);
        assert_eq!(search(&pool, "order:created_at_asc").await, ids);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn pools_and_pool_order(pool: PgPool) {
        let mut ids = Vec::new();
        for _ in 0..4 {
            ids.push(
                seed(
                    &pool,
                    Seed {
                        tags: &["x"],
                        ..Seed::default()
                    },
                )
                .await,
            );
        }
        let contents = |name: &str, post_ids: Vec<i64>, is_deleted| crate::pools::Contents {
            name: name.into(),
            description: String::new(),
            category: "series".into(),
            is_deleted,
            post_ids,
        };
        let comic = crate::pools::create(
            &pool,
            &contents("Comic", vec![ids[2], ids[0], ids[1]], false),
            None,
        )
        .await
        .unwrap();
        crate::pools::create(&pool, &contents("Gone", vec![ids[3]], true), None)
            .await
            .unwrap();

        assert_eq!(search(&pool, "pool:comic").await, [ids[2], ids[1], ids[0]]);
        assert_eq!(
            search(&pool, &format!("pool:{comic}")).await,
            [ids[2], ids[1], ids[0]]
        );
        assert_eq!(search(&pool, "-pool:comic").await, [ids[3]]);
        assert_eq!(search(&pool, "pool:any").await, [ids[2], ids[1], ids[0]]);
        assert_eq!(search(&pool, "pool:none").await, [ids[3]]);
        // Deleted pools match nothing.
        assert!(search(&pool, "pool:gone").await.is_empty());
        assert!(search(&pool, "pool:nothing").await.is_empty());
        assert_eq!(
            search(&pool, "x ordpool:comic").await,
            [ids[2], ids[0], ids[1]]
        );
        assert!(search(&pool, "ordpool:gone").await.is_empty());

        let query = Query::parse("ordpool:comic").unwrap();
        let plan = Plan::resolve(&pool, &query, &public(), &SearchConfig::default())
            .await
            .unwrap();
        assert_eq!(plan.count(&pool).await.unwrap(), Count::Exact(3));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn saved_searches(pool: PgPool) {
        let cat = seed(
            &pool,
            Seed {
                tags: &["cat"],
                ..Seed::default()
            },
        )
        .await;
        let dog = seed(
            &pool,
            Seed {
                tags: &["dog"],
                ..Seed::default()
            },
        )
        .await;
        let tree = seed(
            &pool,
            Seed {
                tags: &["tree"],
                ..Seed::default()
            },
        )
        .await;
        let alice: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT 'alice', id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let pets = ["pets".to_owned()];
        crate::saved_searches::save(&pool, alice, "cat", &pets)
            .await
            .unwrap();
        crate::saved_searches::save(&pool, alice, "dog order:score", &pets)
            .await
            .unwrap();
        crate::saved_searches::save(&pool, alice, "tree", &[])
            .await
            .unwrap();
        // Would loop.
        crate::saved_searches::save(&pool, alice, "search:all", &pets)
            .await
            .unwrap();

        let as_alice = Visibility {
            hidden_tags: Vec::new(),
            viewer: Some(alice),
            ..public()
        };
        assert_eq!(search_as(&pool, "search:pets", &as_alice).await, [dog, cat]);
        assert_eq!(
            search_as(&pool, "search:all", &as_alice).await,
            [tree, dog, cat]
        );
        assert_eq!(search_as(&pool, "-search:pets", &as_alice).await, [tree]);
        assert_eq!(search_as(&pool, "search:pets -dog", &as_alice).await, [cat]);
        assert!(
            search_as(&pool, "search:nothing", &as_alice)
                .await
                .is_empty()
        );
        // Visitors have no saved searches.
        assert!(search(&pool, "search:all").await.is_empty());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn favorite_groups(pool: PgPool) {
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(seed(&pool, Seed::default()).await);
        }
        let user = |name: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
                )
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let (alice, bob) = (user("alice").await, user("bob").await);
        let group = |name: &str, is_public, post_ids: Vec<i64>| crate::favorite_groups::Contents {
            name: name.into(),
            is_public,
            post_ids,
        };
        let best = crate::favorite_groups::create(
            &pool,
            alice,
            &group("Best", true, vec![ids[0], ids[2]]),
        )
        .await
        .unwrap();
        let hidden =
            crate::favorite_groups::create(&pool, alice, &group("Secret", false, vec![ids[1]]))
                .await
                .unwrap();
        let as_user = |viewer| Visibility {
            viewer: Some(viewer),
            ..public()
        };

        // By name: the viewer's own group.
        assert_eq!(
            search_as(&pool, "favgroup:best", &as_user(alice)).await,
            [ids[2], ids[0]]
        );
        assert!(
            search_as(&pool, "favgroup:best", &as_user(bob))
                .await
                .is_empty()
        );
        // By id: public groups for anyone, private ones for their owner.
        assert_eq!(
            search(&pool, &format!("favgroup:{best}")).await,
            [ids[2], ids[0]]
        );
        assert!(
            search(&pool, &format!("favgroup:{hidden}"))
                .await
                .is_empty()
        );
        assert_eq!(
            search_as(&pool, &format!("favgroup:{hidden}"), &as_user(alice)).await,
            [ids[1]]
        );
        assert_eq!(search(&pool, &format!("-favgroup:{best}")).await, [ids[1]]);
        assert_eq!(
            search_as(&pool, "ordfavgroup:best", &as_user(alice)).await,
            [ids[0], ids[2]]
        );
        // Any or none of the viewer's own groups, private ones included.
        assert_eq!(
            search_as(&pool, "favgroup:any", &as_user(alice)).await,
            [ids[2], ids[1], ids[0]]
        );
        assert!(
            search_as(&pool, "favgroup:none", &as_user(alice))
                .await
                .is_empty()
        );
        assert!(
            search_as(&pool, "favgroup:any", &as_user(bob))
                .await
                .is_empty()
        );
        assert_eq!(search(&pool, "favgroup:none").await.len(), 3);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn notes(pool: PgPool) {
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(seed(&pool, Seed::default()).await);
        }
        let note_box = moekura_core::notes::NoteBox {
            x: 0,
            y: 0,
            width: 5,
            height: 5,
        };
        crate::notes::create(&pool, ids[0], note_box, "Good morning, everyone", None)
            .await
            .unwrap();
        crate::notes::create(&pool, ids[0], note_box, "Bye", None)
            .await
            .unwrap();
        let gone = crate::notes::create(&pool, ids[2], note_box, "Good night", None)
            .await
            .unwrap();
        crate::notes::create(&pool, ids[2], note_box, "Later", None)
            .await
            .unwrap();
        let deleted = crate::notes::Changes {
            is_active: Some(false),
            ..crate::notes::Changes::default()
        };
        crate::notes::update(&pool, gone, &deleted, None, None)
            .await
            .unwrap();

        assert_eq!(search(&pool, "note:morning").await, [ids[0]]);
        assert_eq!(search(&pool, "note:good").await, [ids[0]]);
        assert_eq!(search(&pool, "note:good_morning").await, [ids[0]]);
        assert_eq!(search(&pool, "-note:good").await, [ids[2], ids[1]]);
        assert_eq!(search(&pool, "notecount:2").await, [ids[0]]);
        assert_eq!(search(&pool, "notecount:0").await, [ids[1]]);
        assert_eq!(search(&pool, "order:note").await, [ids[2], ids[0]]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn metadata_pixiv_and_embedded_notes(pool: PgPool) {
        let mut ids = Vec::new();
        for _ in 0..3 {
            ids.push(seed(&pool, Seed::default()).await);
        }
        sqlx::query(
            "UPDATE media_assets SET metadata =
                 '{\"File:ColorComponents\": \"1\", \"EXIF:Model\": \"Canon EOS\"}'
             WHERE post_id = $1",
        )
        .bind(ids[0])
        .execute(&pool)
        .await
        .unwrap();
        for (id, source) in [
            (ids[1], "https://www.pixiv.net/en/artworks/12345"),
            (
                ids[2],
                "https://i.pximg.net/img-original/img/2026/01/01/00/00/00/99_p0.png",
            ),
        ] {
            sqlx::query("UPDATE posts SET source = $2 WHERE id = $1")
                .bind(id)
                .bind(source)
                .execute(&pool)
                .await
                .unwrap();
        }
        assert_eq!(search(&pool, "exif:File:ColorComponents=1").await, [ids[0]]);
        assert_eq!(search(&pool, "exif:exif:model").await, [ids[0]]);
        assert_eq!(search(&pool, "exif:EXIF:Model=canon_eos").await, [ids[0]]);
        assert!(
            search(&pool, "exif:File:ColorComponents=3")
                .await
                .is_empty()
        );
        assert_eq!(search(&pool, "pixiv:any").await, [ids[2], ids[1]]);
        assert_eq!(search(&pool, "pixiv:none").await, [ids[0]]);
        assert_eq!(search(&pool, "pixiv_id:12345").await, [ids[1]]);
        assert_eq!(search(&pool, "pixiv:<1000").await, [ids[2]]);

        let note_box = moekura_core::notes::NoteBox {
            x: 0,
            y: 0,
            width: 5,
            height: 5,
        };
        crate::notes::create(&pool, ids[1], note_box, "Hi", None)
            .await
            .unwrap();
        assert!(
            crate::posts::set_embedded_notes(&pool, ids[1], true)
                .await
                .unwrap()
        );
        // Embedded, but with no notes: not counted.
        crate::posts::set_embedded_notes(&pool, ids[2], true)
            .await
            .unwrap();
        assert_eq!(search(&pool, "embedded:true").await, [ids[1]]);
        assert_eq!(search(&pool, "embedded:false").await, [ids[2], ids[0]]);
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn tagger_suggestions(pool: PgPool) {
        let tagged = seed(
            &pool,
            Seed {
                tags: &["cat"],
                ..Seed::default()
            },
        )
        .await;
        let untagged = seed(&pool, Seed::default()).await;
        let plain = seed(&pool, Seed::default()).await;
        let cat = crate::tags::by_name(&pool, "cat").await.unwrap().unwrap();
        let mut conn = pool.acquire().await.unwrap();
        for post_id in [tagged, untagged] {
            crate::tag_suggestions::save(
                &mut conn,
                &crate::tag_suggestions::NewResult {
                    post_id,
                    model: "m",
                    rating: moekura_core::posts::Rating::General,
                    rating_confidence: 0.9,
                    suggestions: &[(cat.id, 0.8)],
                },
            )
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('alias', 'kitty', 'cat', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Suggested but not applied.
        assert_eq!(search(&pool, "ai:cat").await, [untagged]);
        assert_eq!(search(&pool, "ai:kitty").await, [untagged]);
        assert_eq!(search(&pool, "-ai:cat").await, [plain, tagged]);
        assert!(search(&pool, "ai:dog").await.is_empty());
        assert_eq!(search(&pool, "-ai:dog").await.len(), 3);
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
    async fn date_filters_page_in_id_order(pool: PgPool) {
        let mut ids = Vec::new();
        for days_ago in [400, 300, 20, 10, 5, 1] {
            ids.push(
                seed(
                    &pool,
                    Seed {
                        days_ago,
                        ..Seed::default()
                    },
                )
                .await,
            );
        }
        // Two posts uploaded in the same instant: id breaks the tie.
        sqlx::query("UPDATE posts SET created_at = (SELECT created_at FROM posts WHERE id = $1) WHERE id = $2")
            .bind(ids[4])
            .bind(ids[3])
            .execute(&pool)
            .await
            .unwrap();
        let since = (time::OffsetDateTime::now_utc() - time::Duration::days(30)).date();
        let config = SearchConfig::default();
        let pages = |input: String, page: PageRef| {
            let pool = pool.clone();
            let config = config.clone();
            async move {
                let query = Query::parse(&input).unwrap();
                let plan = Plan::resolve(&pool, &query, &public(), &config)
                    .await
                    .unwrap();
                plan.ids(&pool, page).await.unwrap()
            }
        };
        let by = |order: &[usize]| order.iter().map(|&i| ids[i]).collect::<Vec<_>>();
        let recent = format!("date:>={since}");
        assert_eq!(
            pages(recent.clone(), PageRef::default()).await,
            by(&[5, 4, 3, 2])
        );
        let two = format!("{recent} limit:2");
        assert_eq!(
            pages(two.clone(), PageRef::Before(ids[4])).await,
            by(&[3, 2])
        );
        assert_eq!(
            pages(two.clone(), PageRef::After(ids[2])).await,
            by(&[4, 3])
        );
        let ascending = format!("{recent} limit:2 order:id_asc");
        assert_eq!(
            pages(ascending.clone(), PageRef::default()).await,
            by(&[2, 3])
        );
        assert_eq!(pages(ascending, PageRef::After(ids[3])).await, by(&[4, 5]));
        // A cursor whose post is gone still pages from where it was.
        sqlx::query("DELETE FROM posts WHERE id = $1")
            .bind(ids[4])
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(pages(two, PageRef::Before(ids[4])).await, by(&[3, 2]));
        // Excluding a range doesn't change the order.
        assert_eq!(
            pages(format!("-{recent}"), PageRef::default()).await,
            by(&[1, 0])
        );
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
            filters: Vec::new(),
            ordfav: None,
            ordpool: None,
            ordfavgroup: None,
            ordcategory: None,
            custom: Vec::new(),
            statuses: vec!["active"],
            own_pending: None,
            ratings: Vec::new(),
            order,
            per_page: 40,
            total,
            max_page: 1000,
            count_limit: 10_000,
            count_cost_limit: 25_000,
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
            plan.ids_query(PageRef::default(), "")
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
        let mut query = plan.ids_query(PageRef::default(), "").unwrap();
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
