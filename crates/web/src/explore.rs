//! Popular and most viewed posts, and popular and missed searches, under
//! `/explore/posts`; and counting the views and searches they come from.
//!
//! Each web server counts post page views and plain tag searches in
//! memory, each visitor once a day, and adds them to the database every
//! minute ([`flush_every_minute`]), so counting costs a page nothing.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, RandomState};
use std::sync::Mutex;
use std::time::Duration;

use axum::Router;
use axum::extract::Query;
use axum::response::Response;
use axum::routing::get;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::search::{Query as SearchQuery, TagTerm};
use moekura_db::explore::{self, Searched};
use moekura_db::posts;
use moekura_db::search::{PageRef, Plan, SearchError};
use serde::Deserialize;
use time::{Date, Month, OffsetDateTime};

use crate::AppState;
use crate::auth::{CurrentUser, RequestInfo};
use crate::error::AppError;
use crate::pages::Page;
use crate::templates::{search_url, url_value};

/// Visitor and item pairs remembered to count each once a day; past
/// this many, the memory starts over (and some are counted twice).
const MAX_SEEN: usize = 100_000;

/// Days of counts kept.
const KEEP_DAYS: i64 = 400;

/// Searches listed on the searches pages.
const SEARCHES_SHOWN: i64 = 100;

/// Most tags in a search that is counted.
const MAX_COUNTED_TERMS: usize = 6;

/// Views and searches counted since the last flush.
#[derive(Default)]
pub struct Tallies {
    counts: Mutex<Counts>,
    hasher: RandomState,
}

#[derive(Default)]
struct Counts {
    /// The day `seen` is for.
    day: Option<Date>,
    seen: HashSet<u64>,
    views: HashMap<(Date, i64), i32>,
    searches: HashMap<(Date, String), (i32, i32)>,
}

impl Counts {
    /// Whether `key` wasn't seen yet today.
    fn first_today(&mut self, today: Date, key: u64) -> bool {
        if self.day != Some(today) || self.seen.len() >= MAX_SEEN {
            self.day = Some(today);
            self.seen.clear();
        }
        self.seen.insert(key)
    }
}

/// Crawlers and link previews aren't people looking.
fn is_bot(info: &RequestInfo) -> bool {
    let Some(agent) = info.user_agent.as_deref() else {
        return true;
    };
    let agent = agent.to_ascii_lowercase();
    ["bot", "spider", "crawl", "slurp", "preview", "curl", "wget"]
        .iter()
        .any(|word| agent.contains(word))
}

/// Who is looking: their account, or else their address.
fn visitor(current: &CurrentUser, info: &RequestInfo) -> Option<String> {
    match (&current.user, info.ip) {
        (Some(user), _) => Some(format!("u{}", user.id)),
        (None, Some(ip)) => Some(format!("a{ip}")),
        (None, None) => None,
    }
}

/// A search worth counting: plain tags only (no metatags, which may name
/// people, and no groups), in a fixed order. `None` for anything else.
pub(crate) fn counted_search(query: &SearchQuery) -> Option<String> {
    if query.all.is_empty() && query.any.is_empty()
        || !query.conditions.is_empty()
        || !query.groups.is_empty()
        || query.order.is_some()
        || query.term_count() > MAX_COUNTED_TERMS
    {
        return None;
    }
    let term = |prefix: &str, t: &TagTerm| format!("{prefix}{t}");
    let mut terms: Vec<String> = query.all.iter().map(|t| term("", t)).collect();
    terms.extend(query.any.iter().map(|t| term("~", t)));
    terms.extend(query.none.iter().map(|t| term("-", t)));
    terms.sort();
    terms.dedup();
    Some(terms.join(" "))
}

impl Tallies {
    fn today() -> Date {
        OffsetDateTime::now_utc().date()
    }

    /// Counts a view of post `post_id`'s page.
    pub fn view(&self, current: &CurrentUser, info: &RequestInfo, post_id: i64) {
        if is_bot(info) {
            return;
        }
        let Some(who) = visitor(current, info) else {
            return;
        };
        let today = Self::today();
        let key = self.hasher.hash_one(("view", &who, post_id));
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if counts.first_today(today, key) {
            *counts.views.entry((today, post_id)).or_default() += 1;
        }
    }

    /// Counts a search (see [`counted_search`]) that `found` posts or not.
    pub fn search(&self, current: &CurrentUser, info: &RequestInfo, query: &str, found: bool) {
        if is_bot(info) {
            return;
        }
        let Some(who) = visitor(current, info) else {
            return;
        };
        let today = Self::today();
        let key = self.hasher.hash_one(("search", &who, query));
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if counts.first_today(today, key) {
            let entry = counts
                .searches
                .entry((today, query.to_owned()))
                .or_default();
            entry.0 += 1;
            if !found {
                entry.1 += 1;
            }
        }
    }

    /// Takes what was counted, leaving nothing.
    fn take(&self) -> (Vec<(Date, i64, i32)>, Vec<Searched>) {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let views = std::mem::take(&mut counts.views)
            .into_iter()
            .map(|((day, post), n)| (day, post, n))
            .collect();
        let searches = std::mem::take(&mut counts.searches)
            .into_iter()
            .map(|((day, query), (searches, misses))| Searched {
                day,
                query,
                searches,
                misses,
            })
            .collect();
        (views, searches)
    }
}

/// Adds the views and searches counted so far to the database.
pub async fn flush(state: &AppState) {
    let (views, searches) = state.tallies.take();
    let db = state.db.primary();
    if let Err(error) = explore::add_views(db, &views).await {
        tracing::warn!(%error, lost = views.len(), "could not save post view counts");
    }
    if let Err(error) = explore::add_searches(db, &searches).await {
        tracing::warn!(%error, lost = searches.len(), "could not save search counts");
    }
}

/// Forgets counts older than [`KEEP_DAYS`].
pub async fn prune(state: &AppState) {
    let before = OffsetDateTime::now_utc().date() - time::Duration::days(KEEP_DAYS);
    if let Err(error) = explore::prune(state.db.primary(), before).await {
        tracing::warn!(%error, "could not prune old view and search counts");
    }
}

/// Runs [`flush`] every minute, for `moekura serve`.
pub async fn flush_every_minute(state: AppState) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        flush(&state).await;
    }
}

// ---- ranges ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scale {
    Day,
    Week,
    Month,
}

impl Scale {
    fn name(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }
}

/// The days a page covers: a day, the week ending on it, or its month.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Range {
    pub date: Date,
    pub scale: Scale,
    today: Date,
}

const DATE_FORMAT: &[time::format_description::BorrowedFormatItem<'_>] =
    time::macros::format_description!("[year]-[month]-[day]");

fn iso(date: Date) -> String {
    date.format(DATE_FORMAT).unwrap_or_default()
}

fn first_of_month(date: Date) -> Date {
    date.replace_day(1).unwrap_or(date)
}

impl Range {
    /// From `date` (like `2026-01-31`, by default today; anything after
    /// the date's first 10 characters is ignored, as Danbooru sends
    /// times) and `scale` (`day`, the default, `week` or `month`).
    pub fn parse(date: &str, scale: &str, today: Date) -> Result<Self, AppError> {
        let bad_date = || AppError::BadRequest("`date` must look like 2026-01-31".into());
        let date = match date.trim() {
            "" => today,
            text => Date::parse(text.get(..10).ok_or_else(bad_date)?, DATE_FORMAT)
                .map_err(|_| bad_date())?,
        };
        let scale = match scale {
            "" | "day" => Scale::Day,
            "week" => Scale::Week,
            "month" => Scale::Month,
            _ => {
                return Err(AppError::BadRequest(
                    "`scale` must be day, week or month".into(),
                ));
            }
        };
        Ok(Self { date, scale, today })
    }

    /// The first and last day covered.
    pub fn days(&self) -> (Date, Date) {
        match self.scale {
            Scale::Day => (self.date, self.date),
            Scale::Week => (self.date - time::Duration::days(6), self.date),
            Scale::Month => {
                let first = first_of_month(self.date);
                let last = first_of_month(first + time::Duration::days(31)) - time::Duration::DAY;
                (first, last)
            }
        }
    }

    /// The search for the best scored posts in the range.
    pub fn popular_search(&self) -> String {
        let (from, to) = self.days();
        let dates = if from == to {
            iso(from)
        } else {
            format!("{}..{}", iso(from), iso(to))
        };
        format!("date:{dates} order:score")
    }

    fn step(&self, forward: bool) -> Date {
        let sign = if forward { 1 } else { -1 };
        match self.scale {
            Scale::Day => self.date + time::Duration::days(sign),
            Scale::Week => self.date + time::Duration::days(7 * sign),
            Scale::Month => {
                let (year, month) = match (forward, self.date.month()) {
                    (true, Month::December) => (self.date.year() + 1, Month::January),
                    (true, month) => (self.date.year(), month.next()),
                    (false, Month::January) => (self.date.year() - 1, Month::December),
                    (false, month) => (self.date.year(), month.previous()),
                };
                let day = self.date.day().min(month.length(year));
                Date::from_calendar_date(year, month, day).unwrap_or(self.date)
            }
        }
    }

    fn label(&self) -> String {
        let long = time::macros::format_description!("[day padding:none] [month repr:long] [year]");
        let day = |d: Date| d.format(long).unwrap_or_default();
        match self.scale {
            Scale::Day => day(self.date),
            Scale::Week => {
                let (from, to) = self.days();
                format!("{} to {}", day(from), day(to))
            }
            Scale::Month => self
                .date
                .format(time::macros::format_description!(
                    "[month repr:long] [year]"
                ))
                .unwrap_or_default(),
        }
    }

    /// The page for `date` and `scale`, on `path`.
    fn url(path: &str, date: Date, scale: Scale) -> Value {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("date", &iso(date))
            .append_pair("scale", scale.name())
            .finish();
        url_value(&format!("{path}?{query}"))
    }

    /// Links to the other scales and the ranges before and after.
    fn context(&self, path: &str) -> Value {
        let next = self.step(true);
        context! {
            label => self.label(),
            scale => self.scale.name(),
            scales => [Scale::Day, Scale::Week, Scale::Month]
                .iter()
                .map(|&scale| context! {
                    name => scale.name(),
                    url => Self::url(path, self.date, scale),
                })
                .collect::<Vec<_>>(),
            previous_url => Self::url(path, self.step(false), self.scale),
            next_url => (next <= self.today).then(|| Self::url(path, next, self.scale)),
        }
    }
}

// ---- pages ----------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/explore/posts/popular", get(popular))
        .route("/explore/posts/viewed", get(viewed))
        .route("/explore/posts/searches", get(searches))
        .route("/explore/posts/missed_searches", get(missed_searches))
}

#[derive(Debug, Default, Deserialize)]
struct RangeQuery {
    #[serde(default)]
    date: String,
    #[serde(default)]
    scale: String,
}

fn range(params: &RangeQuery) -> Result<Range, AppError> {
    Range::parse(
        &params.date,
        &params.scale,
        OffsetDateTime::now_utc().date(),
    )
}

/// The best scored posts of the range, as ids, at most `limit`.
pub(crate) async fn popular_ids(
    state: &AppState,
    current: &CurrentUser,
    range: &Range,
    limit: u32,
    page: PageRef,
) -> Result<Vec<i64>, AppError> {
    let db = state.reader(current);
    let mut query = SearchQuery::parse(&range.popular_search())
        .map_err(|e| AppError::Internal(e.to_string()))?;
    query.limit = Some(limit);
    let visible = crate::posts::visibility(current);
    let plan = match Plan::resolve(db, &query, &visible, &state.config.search).await {
        Ok(plan) => plan,
        Err(SearchError::Invalid(message)) => return Err(AppError::BadRequest(message)),
        Err(SearchError::Db(error)) => return Err(error.into()),
    };
    match plan.ids(db, page).await {
        Ok(ids) => Ok(ids),
        Err(SearchError::Invalid(message)) => Err(AppError::BadRequest(message)),
        Err(SearchError::Db(error)) => Err(error.into()),
    }
}

/// The most viewed posts of the range that `current` may see, as ids,
/// at most `limit`.
pub(crate) async fn viewed_ids(
    state: &AppState,
    current: &CurrentUser,
    range: &Range,
    limit: u32,
) -> Result<Vec<i64>, AppError> {
    let db = state.reader(current);
    let (from, to) = range.days();
    // Some may be hidden from this viewer; ask for extra.
    let top = explore::most_viewed(db, from, to, i64::from(limit) * 2).await?;
    let ids: Vec<i64> = top.iter().map(|(id, _)| *id).collect();
    let visible = crate::posts::visibility(current);
    let found = posts::by_ids(db, &ids).await?;
    Ok(ids
        .into_iter()
        .filter(|id| found.iter().any(|p| p.id == *id && visible.allows(p)))
        .take(limit as usize)
        .collect())
}

/// The searches made most in the range, or with `missed`, those that
/// most often found nothing.
pub(crate) async fn top_searches(
    state: &AppState,
    current: &CurrentUser,
    range: &Range,
    missed: bool,
    limit: i64,
) -> Result<Vec<(String, i64)>, AppError> {
    let (from, to) = range.days();
    Ok(explore::top_searches(state.reader(current), from, to, missed, limit).await?)
}

fn render_posts(page: &Page, range: &Range, kind: &str, cards: Vec<Value>) -> Response {
    let path = format!("/explore/posts/{kind}");
    page.render(
        "explore.html",
        context! {
            kind => kind,
            range => range.context(&path),
            cards => cards,
        },
    )
}

async fn popular(page: Page, Query(params): Query<RangeQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let range = range(&params)?;
    let state = page.state();
    let limit = state.config.search.per_page;
    let ids = popular_ids(state, &page.current, &range, limit, PageRef::default()).await?;
    let cards = crate::posts::grid(&page, state.reader(&page.current), &ids, None).await?;
    Ok(render_posts(
        &page,
        &range,
        "popular",
        cards.into_iter().map(|(_, card)| card).collect(),
    ))
}

async fn viewed(page: Page, Query(params): Query<RangeQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let range = range(&params)?;
    let state = page.state();
    let limit = state.config.search.per_page;
    let ids = viewed_ids(state, &page.current, &range, limit).await?;
    let cards = crate::posts::grid(&page, state.reader(&page.current), &ids, None).await?;
    Ok(render_posts(
        &page,
        &range,
        "viewed",
        cards.into_iter().map(|(_, card)| card).collect(),
    ))
}

async fn render_searches(
    page: Page,
    params: RangeQuery,
    missed: bool,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let range = range(&params)?;
    let kind = if missed {
        "missed_searches"
    } else {
        "searches"
    };
    let found = top_searches(page.state(), &page.current, &range, missed, SEARCHES_SHOWN).await?;
    Ok(page.render(
        "explore.html",
        context! {
            kind => kind,
            range => range.context(&format!("/explore/posts/{kind}")),
            searches => found
                .iter()
                .map(|(query, count)| context! {
                    query => query,
                    url => Value::from_safe_string(search_url(query)),
                    count => count,
                })
                .collect::<Vec<_>>(),
        },
    ))
}

async fn searches(page: Page, Query(params): Query<RangeQuery>) -> Result<Response, AppError> {
    render_searches(page, params, false).await
}

async fn missed_searches(
    page: Page,
    Query(params): Query<RangeQuery>,
) -> Result<Response, AppError> {
    render_searches(page, params, true).await
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;
    use time::macros::date;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    #[test]
    fn ranges() {
        let today = date!(2026 - 03 - 10);
        let search = |d: &str, s: &str| Range::parse(d, s, today).unwrap().popular_search();
        assert_eq!(search("", ""), "date:2026-03-10 order:score");
        assert_eq!(
            search("2026-01-31T00:00:00Z", "week"),
            "date:2026-01-25..2026-01-31 order:score"
        );
        assert_eq!(
            search("2024-02-10", "month"),
            "date:2024-02-01..2024-02-29 order:score"
        );
        assert!(Range::parse("soon", "day", today).is_err());
        assert!(Range::parse("", "year", today).is_err());

        let month = Range::parse("2026-01-31", "month", today).unwrap();
        assert_eq!(month.step(true), date!(2026 - 02 - 28));
        assert_eq!(month.step(false), date!(2025 - 12 - 31));
        assert_eq!(month.label(), "January 2026");
        let day = Range::parse("", "", today).unwrap();
        assert_eq!(day.label(), "10 March 2026");
        assert!(day.context("/x").get_attr("next_url").unwrap().is_none());
    }

    #[test]
    fn only_plain_tag_searches_count() {
        let counted = |q: &str| counted_search(&SearchQuery::parse(q).unwrap());
        assert_eq!(
            counted("tail cat -dog ~fox ~wolf"),
            Some("-dog cat tail ~fox ~wolf".into())
        );
        assert_eq!(counted("long_*"), Some("long_*".into()));
        for skipped in ["", "fav:alice", "cat rating:s", "cat order:score", "-dog"] {
            assert_eq!(counted(skipped), None, "{skipped}");
        }
    }

    #[test]
    fn counts_each_visitor_once_a_day() {
        let tallies = Tallies::default();
        let alice = CurrentUser::anonymous(&moekura_db::site_cache::SiteSnapshot::new(
            Default::default(),
            Vec::new(),
        ));
        let person = |ip: [u8; 4], agent: &str| RequestInfo {
            user_agent: Some(agent.into()),
            ip: Some(std::net::IpAddr::from(ip)),
        };
        let firefox = "Mozilla/5.0 Firefox/140.0";
        tallies.view(&alice, &person([1, 1, 1, 1], firefox), 7);
        tallies.view(&alice, &person([1, 1, 1, 1], firefox), 7);
        tallies.view(&alice, &person([2, 2, 2, 2], firefox), 7);
        tallies.view(&alice, &person([3, 3, 3, 3], "Discordbot/2.0"), 7);
        tallies.search(&alice, &person([1, 1, 1, 1], firefox), "dgo", false);
        tallies.search(&alice, &person([1, 1, 1, 1], firefox), "cat", true);
        let (views, searches) = tallies.take();
        assert_eq!(views.len(), 1);
        assert_eq!((views[0].1, views[0].2), (7, 2));
        assert_eq!(searches.len(), 2);
        let dgo = searches.iter().find(|s| s.query == "dgo").unwrap();
        assert_eq!((dgo.searches, dgo.misses), (1, 1));
        assert!(tallies.take().0.is_empty());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn pages(pool: PgPool) {
        let state = test_state(&pool).await;
        let app = TestApp::with_peer(
            state.clone(),
            routes().merge(crate::danbooru::test_support::routes()),
            "198.51.100.7:4000".parse().unwrap(),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let mut ids = Vec::new();
        for (width, score) in [(20, 1), (22, 5), (24, 9)] {
            let id = crate::danbooru::test_support::upload(&app, &alice, width, "cat").await;
            sqlx::query("UPDATE posts SET score = $2 WHERE id = $1")
                .bind(id)
                .bind(score)
                .execute(&pool)
                .await
                .unwrap();
            ids.push(id);
        }
        let today = OffsetDateTime::now_utc().date();
        explore::add_views(
            &pool,
            &[(today, ids[0], 9), (today, ids[1], 2), (today, ids[2], 20)],
        )
        .await
        .unwrap();
        explore::add_searches(
            &pool,
            &[Searched {
                day: today,
                query: "dgo".into(),
                searches: 4,
                misses: 4,
            }],
        )
        .await
        .unwrap();

        let popular = app.get("/explore/posts/popular", Some(&alice)).await;
        assert_eq!(popular.status, StatusCode::OK);
        let at = |body: &str, id: i64| body.find(&format!("href=\"/posts/{id}\""));
        let (low, high, top) = (
            at(&popular.body, ids[0]).expect(&popular.body),
            at(&popular.body, ids[1]).unwrap(),
            at(&popular.body, ids[2]).unwrap(),
        );
        assert!(top < high && high < low);
        assert!(popular.body.contains("scale=week"), "{}", popular.body);

        let viewed = app
            .get("/explore/posts/viewed?scale=week", Some(&alice))
            .await;
        let (most, less) = (
            at(&viewed.body, ids[0]).expect(&viewed.body),
            at(&viewed.body, ids[1]).unwrap(),
        );
        assert!(at(&viewed.body, ids[2]).unwrap() < most && most < less);

        let missed = app.get("/explore/posts/missed_searches", None).await;
        assert!(missed.body.contains("/posts?tags=dgo"), "{}", missed.body);
        let searches = app
            .get("/explore/posts/searches?date=2001-01-01", None)
            .await;
        assert!(!searches.body.contains("dgo"));
        assert_eq!(
            app.get("/explore/posts/viewed?scale=year", None)
                .await
                .status,
            StatusCode::BAD_REQUEST
        );

        // Page views and searches are counted, and saved by a flush.
        let browser = [("user-agent", "Mozilla/5.0 Firefox/140.0")];
        let post = format!("/posts/{}", ids[1]);
        app.get_with_headers(&post, &browser).await;
        app.get_with_headers("/posts?tags=nothing_here", &browser)
            .await;
        flush(&state).await;
        let top = explore::most_viewed(&pool, today, today, 10).await.unwrap();
        assert!(top.contains(&(ids[1], 3)), "{top:?}");
        let missed = explore::top_searches(&pool, today, today, true, 10)
            .await
            .unwrap();
        assert!(
            missed.contains(&("nothing_here".to_owned(), 1)),
            "{missed:?}"
        );
    }
}
