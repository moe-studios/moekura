//! The post grid (front page) and single post pages.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::posts::{PostStatus, Rating};
use moekura_core::search::{Order, Query as SearchQuery, TagTerm};
use moekura_core::user_settings::UserSettings;
use moekura_db::media::{self, Variant};
use moekura_db::posts::{self, Card, Post, Visibility};
use moekura_db::search::{Count, PageRef, Plan, SearchError};
use moekura_db::{tags, users};
use moekura_storage::Key;
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::pages::Page;
use crate::templates::{search_url, url_value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/posts", get(index))
        .route("/posts/{id}", get(show))
        .route("/posts/{id}/next", get(next))
        .route("/posts/{id}/prev", get(previous))
}

/// Refuses a change to `lock`'s part of `post` if it's locked and
/// `current` can't change locked posts.
pub(crate) fn check_lock(
    current: &CurrentUser,
    post: &posts::Post,
    lock: moekura_core::posts::PostLock,
) -> Result<(), AppError> {
    if post.is_locked(lock) && !current.can(Permission::LockPosts) {
        return Err(AppError::BadRequest(locked_message(lock)));
    }
    Ok(())
}

/// What a refused change to a locked part of a post says.
pub(crate) fn locked_message(lock: moekura_core::posts::PostLock) -> String {
    use moekura_core::posts::PostLock;
    match lock {
        PostLock::Rating => "This post's rating is locked.",
        PostLock::Tags => "This post's tags are locked.",
        PostLock::Notes => "This post's notes are locked.",
        PostLock::Status => "This post's status is locked.",
    }
    .to_owned()
}

/// Whether a file of `media_type` is played as a video: videos, and
/// ugoira (as the video made of their frames).
pub(crate) fn is_video(media_type: &str) -> bool {
    matches!(media_type, "mp4" | "webm" | "ugoira")
}

/// Which posts `current` may see.
pub fn visibility(current: &CurrentUser) -> Visibility {
    let mut statuses = vec![PostStatus::Active, PostStatus::Flagged];
    if current.can(Permission::ApprovePosts) {
        statuses.push(PostStatus::Pending);
    }
    if current.can(Permission::ViewDeleted) {
        statuses.push(PostStatus::Deleted);
    }
    Visibility {
        hidden_tags: current.hidden_tags.clone(),
        deleted_by_default: statuses.contains(&PostStatus::Deleted)
            && current
                .user
                .as_ref()
                .is_some_and(|u| UserSettings::from_json(&u.settings).show_deleted),
        statuses,
        viewer: current.user.as_ref().map(|u| u.id),
        ratings: current.ratings.clone(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    tags: String,
    #[serde(default)]
    page: String,
    /// `off` shows posts the viewer's blacklist would hide.
    #[serde(default)]
    blacklist: String,
}

/// Tags listed beside search results.
const SIDEBAR_TAGS: usize = 25;

/// Tags of a search that found nothing checked for "did you mean".
const DID_YOU_MEAN_TERMS: usize = 6;

/// Replacements offered for each tag.
const DID_YOU_MEAN_EACH: i64 = 3;

/// Search results; the front page is the empty search.
async fn index(
    page: Page,
    info: crate::auth::RequestInfo,
    Query(params): Query<IndexQuery>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.reader(&page.current);
    // The user's page size, within the site's limit.
    let config = &state.search_config_for(&page.current);
    let input = params.tags.trim();
    let page_ref: PageRef = if params.page.is_empty() {
        PageRef::default()
    } else {
        params
            .page
            .parse()
            .map_err(|()| AppError::BadRequest("That page doesn't exist".into()))?
    };

    let failed = |message: String| {
        page.render_with_status(
            StatusCode::BAD_REQUEST,
            "posts.html",
            context! { search => context! { tags => input, error => message } },
        )
    };
    let query = match SearchQuery::parse(input) {
        Ok(query) => query,
        Err(error) => return Ok(failed(error.to_string())),
    };
    let mut plan = match Plan::resolve(db, &query, &visibility(&page.current), config).await {
        Ok(plan) => plan,
        Err(SearchError::Invalid(message)) => return Ok(failed(message)),
        Err(SearchError::Db(error)) => return Err(error.into()),
    };
    // Blacklisted posts are left out by the search itself, or blurred
    // among the results, with a link to show them.
    let show_all = params.blacklist == "off";
    let blacklist = crate::blacklist::for_viewer(state, db, &page.current).await?;
    let blur = crate::blacklist::blurs(&page.current);
    let unfiltered = match &blacklist {
        Some(list) if !show_all && !blur => {
            let all = plan.clone();
            plan.exclude(&list.exclusions());
            Some(all)
        }
        _ => None,
    };
    let count = state.counts.count(&plan, db, &page.current).await;
    let (ids, count) = match (plan.ids(db, page_ref).await, count) {
        (Ok(ids), Ok(count)) => (ids, count),
        (Err(SearchError::Invalid(message)), _) => return Ok(failed(message)),
        (Err(SearchError::Db(error)), _) | (_, Err(SearchError::Db(error))) => {
            return Err(error.into());
        }
        (_, Err(SearchError::Invalid(message))) => return Ok(failed(message)),
    };
    // How many the blacklist left out, when both counts are exact.
    let hidden = match (&unfiltered, count) {
        (Some(all), Count::Exact(shown)) => {
            match state.counts.count(all, db, &page.current).await {
                Ok(Count::Exact(all)) => Some(all - shown),
                Ok(_) | Err(SearchError::Invalid(_)) => None,
                Err(SearchError::Db(error)) => return Err(error.into()),
            }
        }
        _ => None,
    };

    if page_ref == PageRef::default()
        && let Some(counted) = crate::explore::counted_search(&query)
    {
        state
            .tallies
            .search(&page.current, &info, &counted, !ids.is_empty());
    }
    let thumbs = Thumbs::for_viewer(state, &page.current);
    let cards = posts::cards(db, &ids, thumbs.kinds()).await?;
    let normalized = query.to_string();
    // Even the empty search, so the post page can step through it.
    let post_query = Some(
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", &normalized)
            .finish(),
    );
    let is_blacklisted = |card: &Card| {
        blur && !show_all
            && blacklist.as_ref().is_some_and(|list| {
                let rating = card.rating.parse().unwrap_or(Rating::Explicit);
                list.matching(rating, &card.tag_ids).is_some()
            })
    };
    let blurred = cards.iter().filter(|card| is_blacklisted(card)).count();
    let blacklist_url = |off: bool| {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if !normalized.is_empty() {
            query.append_pair("tags", &normalized);
        }
        if !params.page.is_empty() {
            query.append_pair("page", &params.page);
        }
        if off {
            query.append_pair("blacklist", "off");
        }
        url_value(&format!("/posts?{}", query.finish()))
    };
    // Unless the counts tell that it left out nothing.
    let left_out = unfiltered.is_some() && hidden != Some(0);
    let blacklisted = context! {
        hidden => hidden.filter(|_| left_out),
        left_out => left_out,
        blurred => blurred,
        show_url => (left_out || blurred > 0).then(|| blacklist_url(true)),
        hide_url => (show_all && blacklist.is_some()).then(|| blacklist_url(false)),
    };
    let labels = card_labels(db, &cards).await?;
    let card_values: Vec<Value> = cards
        .iter()
        .map(|card| {
            let value = card_context(state, card, &labels, thumbs.size, post_query.as_deref());
            if is_blacklisted(card) {
                with_blur(value)
            } else {
                value
            }
        })
        .collect();

    // Blurred posts' tags stay out of the sidebar, as left out ones do.
    let shown: Vec<Card> = cards
        .iter()
        .filter(|card| !is_blacklisted(card))
        .cloned()
        .collect();
    let deleted = deleted_hidden(&page, db, &query, &normalized, config).await?;
    let sidebar = sidebar_tags(db, &shown, &normalized).await?;
    let wiki = crate::wiki::search_excerpt(db, &query).await?;
    // Only for searches that found nothing, so the rest pay nothing.
    let did_you_mean = if ids.is_empty() {
        did_you_mean(db, &query, &normalized).await?
    } else {
        Vec::new()
    };
    let pager = Pager {
        query: &normalized,
        page: page_ref,
        per_page: plan.per_page(),
        max_page: config.max_page,
        keyset: plan.supports_keyset(),
        descending: plan.order() == Order::IdDesc,
        count,
        first: cards.first().map(|c| c.id),
        last: cards.last().map(|c| c.id),
        full: ids.len() == plan.per_page() as usize,
        show_blacklisted: show_all,
    };
    Ok(page.render(
        "posts.html",
        context! {
            search => context! { tags => normalized },
            save_search => crate::saved_searches::save_form(&page.current, &normalized),
            feed_url => url_value(&format!(
                "/posts.atom?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("tags", &normalized)
                    .finish()
            )),
            can_tag_script => page.current.is_logged_in() && page.current.can(Permission::EditPosts),
            cards => card_values,
            blacklisted => blacklisted,
            deleted => deleted,
            count => count_text(count),
            sidebar => sidebar,
            wiki => wiki,
            did_you_mean => did_you_mean,
            pager => pager.context(),
        },
    ))
}

/// How many deleted posts a search left out, with a link to include
/// them, for viewers who may see deleted posts but didn't ask for them.
async fn deleted_hidden(
    page: &Page,
    db: &sqlx::PgPool,
    query: &SearchQuery,
    normalized: &str,
    config: &moekura_core::config::SearchConfig,
) -> Result<Option<Value>, AppError> {
    let visible = visibility(&page.current);
    // A `status:` anywhere, groups included, already decides.
    if !visible.statuses.contains(&PostStatus::Deleted)
        || visible.deleted_by_default
        || query.status().is_some()
        || !query.groups.is_empty()
    {
        return Ok(None);
    }
    let Ok(deleted) = SearchQuery::parse(&format!("{normalized} status:deleted")) else {
        return Ok(None);
    };
    let plan = match Plan::resolve(db, &deleted, &visible, config).await {
        Ok(plan) => plan,
        Err(SearchError::Invalid(_)) => return Ok(None),
        Err(SearchError::Db(error)) => return Err(error.into()),
    };
    let count = match page.state().counts.count(&plan, db, &page.current).await {
        Ok(Count::Exact(0)) | Err(SearchError::Invalid(_)) => return Ok(None),
        Ok(count) => count,
        Err(SearchError::Db(error)) => return Err(error.into()),
    };
    Ok(Some(context! {
        count => count_text(count).replacen(" post", " deleted post", 1),
        show_url => Value::from_safe_string(search_url(format!("{normalized} status:any").trim_start())),
    }))
}

/// For a search that found nothing: its plain tags that match no posts,
/// each with searches that swap it for a tag likely meant instead.
async fn did_you_mean(
    db: &sqlx::PgPool,
    query: &SearchQuery,
    normalized: &str,
) -> Result<Vec<Value>, AppError> {
    // Negated terms, wildcards and metatags aren't tag names to fix.
    let terms = query
        .all
        .iter()
        .map(|term| (term, ""))
        .chain(query.any.iter().map(|term| (term, "~")))
        .filter_map(|(term, prefix)| match term {
            TagTerm::Name(name) => Some((name.as_str(), prefix)),
            TagTerm::Wildcard(_) => None,
        })
        .take(DID_YOU_MEAN_TERMS);
    let mut found = Vec::new();
    for (name, prefix) in terms {
        let replacements = tags::replacements(db, name, DID_YOU_MEAN_EACH).await?;
        if replacements.is_empty() {
            continue;
        }
        // Top-level tags print as words of their own.
        let typed = format!("{prefix}{name}");
        let links: Vec<Value> = replacements
            .iter()
            .map(|replacement| {
                let replaced: Vec<String> = normalized
                    .split(' ')
                    .map(|word| {
                        if word == typed {
                            format!("{prefix}{replacement}")
                        } else {
                            word.to_owned()
                        }
                    })
                    .collect();
                context! {
                    name => replacement,
                    url => Value::from_safe_string(search_url(&replaced.join(" "))),
                }
            })
            .collect();
        found.push(context! { term => name, replacements => links });
    }
    Ok(found)
}

/// The most used tags among `cards`, grouped as on post pages, each with
/// links to narrow or exclude it from `query`.
async fn sidebar_tags(
    db: &sqlx::PgPool,
    cards: &[Card],
    query: &str,
) -> Result<Vec<Value>, AppError> {
    let mut ids: Vec<i32> = cards
        .iter()
        .flat_map(|c| c.tag_ids.iter().copied())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let mut found = tags::by_ids(db, &ids).await?;
    found.truncate(SIDEBAR_TAGS);
    let categories = tags::categories(db).await?;
    Ok(found
        .iter()
        .map(|tag| {
            let category = categories.iter().find(|c| c.id == tag.category_id);
            let with = |term: String| {
                Value::from_safe_string(search_url(format!("{query} {term}").trim()))
            };
            context! {
                name => tag.name,
                count => tag.post_count,
                category => category.map(|c| c.name.clone()),
                url => Value::from_safe_string(search_url(&tag.name)),
                wiki_url => Value::from_safe_string(moekura_core::markup::wiki_url(&tag.name)),
                include_url => with(tag.name.clone()),
                exclude_url => with(format!("-{}", tag.name)),
            }
        })
        .collect())
}

pub(crate) fn count_text(count: Count) -> String {
    let posts = |n: i64| if n == 1 { "post" } else { "posts" };
    match count {
        Count::Exact(n) => format!("{} {}", thousands(n), posts(n)),
        Count::About(n) => format!("about {} {}", thousands(n), posts(n)),
        Count::AtLeast(n) => format!("{}+ posts", thousands(n)),
    }
}

/// `12345` as `12,345`.
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Pagination links for a results page.
struct Pager<'a> {
    query: &'a str,
    page: PageRef,
    per_page: u32,
    max_page: u32,
    /// Whether keyset (`b…`/`a…`) links work for this order.
    keyset: bool,
    /// Id order is newest first.
    descending: bool,
    count: Count,
    /// Ids of the first and last post shown.
    first: Option<i64>,
    last: Option<i64>,
    /// The page was full, so there may be more.
    full: bool,
    /// `blacklist=off`, kept from page to page.
    show_blacklisted: bool,
}

impl Pager<'_> {
    fn url(&self, page: &str) -> Value {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if !self.query.is_empty() {
            query.append_pair("tags", self.query);
        }
        query.append_pair("page", page);
        if self.show_blacklisted {
            query.append_pair("blacklist", "off");
        }
        url_value(&format!("/posts?{}", query.finish()))
    }

    /// Numbered pages there are, as far as known.
    fn pages(&self) -> Option<u32> {
        let per_page = i64::from(self.per_page.max(1));
        let pages = match self.count {
            Count::Exact(n) | Count::About(n) => (n + per_page - 1) / per_page,
            Count::AtLeast(_) => return None,
        };
        Some(u32::try_from(pages).unwrap_or(u32::MAX).min(self.max_page))
    }

    fn context(&self) -> Value {
        // Keyset links move away from the shown posts: "older" is lower
        // ids in newest-first order, higher ids otherwise.
        let (towards_end, towards_start) = if self.descending {
            ('b', 'a')
        } else {
            ('a', 'b')
        };
        let keyset_next = || {
            self.last
                .filter(|_| self.keyset && self.full)
                .map(|id| self.url(&format!("{towards_end}{id}")))
        };
        match self.page {
            PageRef::Number(current) => {
                let pages = self.pages();
                let has_next = match pages {
                    Some(pages) => current < pages,
                    None => self.full,
                };
                let next = if has_next && current < self.max_page {
                    Some(self.url(&(current + 1).to_string()))
                } else if has_next {
                    keyset_next()
                } else {
                    None
                };
                let last = pages.unwrap_or(current).max(current);
                // A window around the current page, plus the first and
                // (when known) the last.
                let mut numbers: Vec<Value> = Vec::new();
                let mut previous = 0;
                for n in 1..=last {
                    let shown = n == 1
                        || (n + 2 >= current && n <= current + 2)
                        || (n == last && pages.is_some());
                    if !shown {
                        continue;
                    }
                    if n > previous + 1 {
                        numbers.push(context! { gap => true });
                    }
                    numbers.push(context! {
                        number => n,
                        url => self.url(&n.to_string()),
                        current => n == current,
                    });
                    previous = n;
                }
                context! {
                    previous => (current > 1).then(|| self.url(&(current - 1).to_string())),
                    next => next,
                    numbers => if last > 1 { numbers } else { Vec::new() },
                }
            }
            PageRef::Before(_) | PageRef::After(_) => context! {
                previous => self.first.map(|id| self.url(&format!("{towards_start}{id}"))),
                next => keyset_next(),
                numbers => Vec::<Value>::new(),
            },
        }
    }
}

/// The URL of a stored file, for templates. Built from a validated key and
/// the configured base URL, so it is marked safe (escaping would turn `/`
/// into `&#x2f;`).
fn file_url(state: &AppState, key: &str) -> Option<Value> {
    Key::parse(key).map(|k| url_value(&state.file_url(&k)))
}

/// The thumbnails grids show a viewer: the smallest size, or the next
/// one up for those who chose large thumbnails.
pub(crate) struct Thumbs {
    /// The box they fit in, in pixels.
    pub size: u32,
    /// Renditions for 1x and 2x screens.
    kinds: (String, String),
}

impl Thumbs {
    pub fn for_viewer(state: &AppState, current: &CurrentUser) -> Self {
        let sizes = &state.media.config().thumbnail_sizes;
        let small = sizes.first().copied().unwrap_or(250);
        let large = sizes.get(1).copied().unwrap_or(small);
        let wants_large = current
            .user
            .as_ref()
            .is_some_and(|u| UserSettings::from_json(&u.settings).large_thumbnails);
        let size = if wants_large { large } else { small };
        Self {
            size,
            kinds: (format!("thumb-{size}"), format!("thumb-{large}")),
        }
    }

    pub fn kinds(&self) -> (&str, &str) {
        (&self.kinds.0, &self.kinds.1)
    }
}

/// A card marked as blacklisted, to be shown blurred.
fn with_blur(card: Value) -> Value {
    context! { ..card, ..context! { blacklisted => true } }
}

/// Cards for the first `limit` posts the search `tags` finds that the
/// viewer may see, after their blacklist; none if the search fails.
pub(crate) async fn preview(page: &Page, tags: &str, limit: u32) -> Result<Vec<Value>, AppError> {
    let state = page.state();
    let db = state.reader(&page.current);
    let Ok(mut query) = SearchQuery::parse(tags) else {
        return Ok(Vec::new());
    };
    query.limit = Some(limit);
    let visible = visibility(&page.current);
    let Ok(mut plan) = Plan::resolve(db, &query, &visible, &state.search_config()).await else {
        return Ok(Vec::new());
    };
    plan.exclude(&crate::blacklist::exclusions(state, db, &page.current).await?);
    let Ok(ids) = plan.ids(db, PageRef::default()).await else {
        return Ok(Vec::new());
    };
    Ok(grid(page, db, &ids, None)
        .await?
        .into_iter()
        .map(|(_, card)| card)
        .collect())
}

/// Grid cards for posts `ids` (their ids and contexts, in order), leaving
/// out posts the viewer may not see and those their blacklist hides, or
/// blurring those if they chose that. `post_query` is added to the post
/// links, as for [`card_context`].
pub(crate) async fn grid(
    page: &Page,
    db: &sqlx::PgPool,
    ids: &[i64],
    post_query: Option<&str>,
) -> Result<Vec<(i64, Value)>, AppError> {
    grid_for(page, db, ids, post_query, &visibility(&page.current)).await
}

/// [`grid`] for the posts `visible` allows, for pages that show the
/// viewer more than they browse with (purging ignores their safe mode).
pub(crate) async fn grid_for(
    page: &Page,
    db: &sqlx::PgPool,
    ids: &[i64],
    post_query: Option<&str>,
    visible: &Visibility,
) -> Result<Vec<(i64, Value)>, AppError> {
    let state = page.state();
    let thumbs = Thumbs::for_viewer(state, &page.current);
    // Whoever passed the ids, a thumbnail leads to the files.
    let ids = posts::visible_ids(db, ids, visible).await?;
    let cards = posts::cards(db, &ids, thumbs.kinds()).await?;
    let blacklist = crate::blacklist::for_viewer(state, db, &page.current).await?;
    let blur = crate::blacklist::blurs(&page.current);
    let labels = card_labels(db, &cards).await?;
    Ok(cards
        .iter()
        .filter_map(|card| {
            let blacklisted = blacklist.as_ref().is_some_and(|list| {
                let rating = card.rating.parse().unwrap_or(Rating::Explicit);
                list.matching(rating, &card.tag_ids).is_some()
            });
            let value = card_context(state, card, &labels, thumbs.size, post_query);
            match (blacklisted, blur) {
                (false, _) => Some((card.id, value)),
                (true, true) => Some((card.id, with_blur(value))),
                (true, false) => None,
            }
        })
        .collect())
}

/// Posts `ids` as a reader shows them: the resized sample of stills, the
/// original of animations and videos, in order. Posts the viewer may not
/// see are left out; those their blacklist hides come without a file.
pub(crate) async fn displays(
    page: &Page,
    db: &sqlx::PgPool,
    ids: &[i64],
) -> Result<Vec<Value>, AppError> {
    let state = page.state();
    let visible = visibility(&page.current);
    let mut found = posts::by_ids(db, ids).await?;
    found.retain(|post| visible.allows(post));
    let assets = media::for_posts(db, ids).await?;
    let asset_ids: Vec<i64> = assets.iter().map(|a| a.id).collect();
    let variants = media::variants_of(db, &asset_ids).await?;
    let blacklist = crate::blacklist::for_viewer(state, db, &page.current).await?;
    let mut shown = Vec::with_capacity(ids.len());
    for id in ids {
        let (Some(post), Some(asset)) = (
            found.iter().find(|p| p.id == *id),
            assets.iter().find(|a| a.post_id == *id),
        ) else {
            continue;
        };
        let blacklisted = blacklist
            .as_ref()
            .and_then(|list| list.matching(post.rating, &post.tag_ids).map(str::to_owned));
        let variant = |kind: &str| {
            variants
                .iter()
                .find(|v| v.asset_id == asset.id && v.kind == kind)
        };
        let video = is_video(&asset.media_type);
        let display = match (variant("sample"), variant("video")) {
            (Some(sample), _) if !video && asset.frames <= 1 => {
                (&sample.storage_key, sample.width, sample.height)
            }
            // An ugoira plays its video.
            (_, Some(played)) => (&played.storage_key, played.width, played.height),
            _ => (&asset.storage_key, asset.width, asset.height),
        };
        shown.push(context! {
            id => post.id,
            blacklisted => blacklisted,
            url => blacklisted.is_none().then(|| file_url(state, display.0)).flatten(),
            poster => variant("poster").and_then(|v| file_url(state, &v.storage_key)),
            video => video,
            width => display.1,
            height => display.2,
        });
    }
    Ok(shown)
}

/// A grid card. `post_query` (`q=…`) is added to the post link so the post
/// page can lead back to the search; `labels` are from [`card_labels`].
fn card_context(
    state: &AppState,
    card: &Card,
    labels: &HashMap<i64, String>,
    box_size: u32,
    post_query: Option<&str>,
) -> Value {
    let url = |key: &Option<String>| key.as_deref().and_then(|k| file_url(state, k));
    let (width, height) = fit(card.width, card.height, box_size);
    let href = match post_query {
        Some(query) => format!("/posts/{}?{query}", card.id),
        None => format!("/posts/{}", card.id),
    };
    context! {
        id => card.id,
        href => url_value(&href),
        thumb => url(&card.thumb),
        thumb_2x => url(&card.thumb_2x),
        width => width,
        height => height,
        rating => card.rating,
        status => card.status,
        has_parent => card.has_parent,
        has_children => card.has_children,
        video => is_video(&card.media_type),
        animated => card.frames > 1,
        duration => card.duration_ms.map(duration),
        sound => card.has_audio,
        label => labels.get(&card.id).filter(|label| !label.is_empty()),
        score => card.score,
        fav_count => card.fav_count,
    }
}

/// What each card shows, in words, for its alt text: its characters,
/// copyrights and artists (two of each, the most used first), or its three
/// most used tags when it has none of those. Keyed by post id.
async fn card_labels(db: &sqlx::PgPool, cards: &[Card]) -> Result<HashMap<i64, String>, AppError> {
    let mut ids: Vec<i32> = cards
        .iter()
        .flat_map(|c| c.tag_ids.iter().copied())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let found = tags::by_ids(db, &ids).await?;
    let by_id: HashMap<i32, &tags::Tag> = found.iter().map(|t| (t.id, t)).collect();
    let categories = tags::categories(db).await?;
    let named: Vec<i16> = ["character", "copyright", "artist"]
        .iter()
        .filter_map(|name| categories.iter().find(|c| c.name == *name).map(|c| c.id))
        .collect();
    Ok(cards
        .iter()
        .map(|card| {
            let mut own: Vec<&tags::Tag> = card
                .tag_ids
                .iter()
                .filter_map(|id| by_id.get(id).copied())
                .collect();
            own.sort_by(|a, b| {
                b.post_count
                    .cmp(&a.post_count)
                    .then_with(|| a.name.cmp(&b.name))
            });
            let mut names: Vec<&str> = named
                .iter()
                .flat_map(|&category| {
                    own.iter()
                        .filter(move |t| t.category_id == category)
                        .take(2)
                        .map(|t| t.name.as_str())
                })
                .collect();
            if names.is_empty() {
                names = own.iter().take(3).map(|t| t.name.as_str()).collect();
            }
            // Read as words, not as one long identifier.
            (card.id, names.join(", ").replace('_', " "))
        })
        .collect())
}

/// A video's or animation's length as `m:ss`, or `h:mm:ss` from an hour.
fn duration(ms: i32) -> String {
    let seconds = ms.max(0) / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Size of a `width`×`height` image scaled down to fit a `size` box.
fn fit(width: i32, height: i32, size: u32) -> (u32, u32) {
    let (w, h) = (width.max(1) as f64, height.max(1) as f64);
    let scale = (f64::from(size) / w.max(h)).min(1.0);
    ((w * scale).round() as u32, (h * scale).round() as u32)
}

#[derive(Debug, Default, Deserialize)]
struct StepQuery {
    #[serde(default)]
    q: String,
}

async fn next(
    page: Page,
    Path(id): Path<i64>,
    Query(params): Query<StepQuery>,
) -> Result<Response, AppError> {
    step(page, id, &params.q, true).await
}

async fn previous(
    page: Page,
    Path(id): Path<i64>,
    Query(params): Query<StepQuery>,
) -> Result<Response, AppError> {
    step(page, id, &params.q, false).await
}

/// Redirects to the post after (or before) `id` in the search `q`, or
/// back to the search at either end.
async fn step(page: Page, id: i64, q: &str, forward: bool) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.reader(&page.current);
    let mut query = SearchQuery::parse(q).map_err(|e| AppError::BadRequest(e.to_string()))?;
    query.limit = Some(1);
    let mut plan = match Plan::resolve(
        db,
        &query,
        &visibility(&page.current),
        &state.search_config(),
    )
    .await
    {
        Ok(plan) => plan,
        Err(SearchError::Invalid(message)) => return Err(AppError::BadRequest(message)),
        Err(SearchError::Db(error)) => return Err(error.into()),
    };
    // Skipping blacklisted posts, as the results did.
    plan.exclude(&crate::blacklist::exclusions(state, db, &page.current).await?);
    // "Next" is further along the display order: lower ids when newest
    // come first.
    let towards_lower = forward == (plan.order() != Order::IdAsc);
    let page_ref = if towards_lower {
        PageRef::Before(id)
    } else {
        PageRef::After(id)
    };
    let found = match plan.ids(db, page_ref).await {
        Ok(ids) => ids.first().copied(),
        Err(SearchError::Invalid(message)) => return Err(AppError::BadRequest(message)),
        Err(SearchError::Db(error)) => return Err(error.into()),
    };
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", q)
        .finish();
    let target = match found {
        Some(post) => format!("/posts/{post}?{encoded}"),
        None => search_url(q),
    };
    Ok(Redirect::to(&target).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct ShowQuery {
    /// The search the post was opened from, possibly empty (all posts).
    q: Option<String>,
    /// `off` shows the post even if the viewer's blacklist matches it.
    #[serde(default)]
    blacklist: String,
    /// A comment to start a reply to.
    reply: Option<i64>,
    /// The pool the post was opened from, for stepping through it.
    pool: Option<i32>,
    /// `1` shows the original image rather than the resized sample, `0`
    /// the sample even for those who chose originals.
    #[serde(default)]
    original: String,
    /// Set after a save, to warn about incomplete tagging.
    #[serde(default)]
    check: String,
    /// Category prefixes that didn't apply (see crate::tag_warnings).
    #[serde(default)]
    kept: String,
}

async fn show(
    page: Page,
    info: crate::auth::RequestInfo,
    Path(id): Path<i64>,
    Query(params): Query<ShowQuery>,
) -> Result<Response, AppError> {
    let comment = match params.reply {
        Some(reply) => crate::comments::reply_draft(page.state(), id, reply).await?,
        None => None,
    };
    let response = render_post(
        &page,
        id,
        params.q.as_deref(),
        params.blacklist == "off",
        Extra {
            comment,
            pool: params.pool,
            original: match params.original.as_str() {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            },
            check: (!params.check.is_empty()).then_some(params.kept.as_str()),
            ..Extra::default()
        },
    )
    .await?;
    if response.status() == StatusCode::OK {
        page.state().tallies.view(&page.current, &info, id);
    }
    Ok(response)
}

/// The edit form as submitted, shown again with an error.
pub(crate) struct FailedEdit<'a> {
    pub form: &'a crate::edit::EditForm,
    pub error: String,
}

/// Text for the new comment form: a reply's quote, or a refused comment.
pub(crate) struct CommentDraft {
    pub body: String,
    pub error: Option<String>,
}

/// What a post page shows besides the post.
#[derive(Default)]
pub(crate) struct Extra<'a> {
    /// Refills the edit form after a rejected edit.
    pub failed_edit: Option<FailedEdit<'a>>,
    pub comment: Option<CommentDraft>,
    /// The pool the post was opened from.
    pub pool: Option<i32>,
    /// Whether to show the original image rather than the resized sample;
    /// `None` for the viewer's setting.
    pub original: Option<bool>,
    /// After a save: warn about incomplete tagging, with the `kept`
    /// parameter.
    pub check: Option<&'a str>,
}

/// The post page.
pub(crate) async fn render_post(
    page: &Page,
    id: i64,
    search: Option<&str>,
    show_blacklisted: bool,
    extra: Extra<'_>,
) -> Result<Response, AppError> {
    let failed = extra.failed_edit;
    page.current.require(Permission::ViewPosts)?;
    let from_search = search.is_some();
    let search = search.unwrap_or_default();
    let state = page.state();
    // The primary, so an uploader redirected here sees their post even if
    // a replica lags.
    let db = state.db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    // Uploaders see their own deleted posts, to know why and to appeal,
    // but can't do anything else with them.
    let me = page.current.user.as_ref().map(|u| u.id);
    let own_deleted = post.status == PostStatus::Deleted && me.is_some() && post.uploader_id == me;
    let limited = !visibility(&page.current).allows(&post);
    if limited && !own_deleted {
        return Err(AppError::NotFound);
    }
    let asset = media::for_post(db, id).await?.ok_or(AppError::NotFound)?;
    let variants = media::variants(db, asset.id).await?;
    let categories = tags::categories(db).await?;
    let post_tags = tags::by_ids(db, &post.tag_ids).await?;
    let mut tag_names: Vec<&str> = post_tags.iter().map(|t| t.name.as_str()).collect();
    tag_names.sort_unstable();
    let tag_string = tag_names.join(" ");
    let tag_groups = crate::tags::grouped(&categories, post_tags.clone());
    let family = family_context(page, &post).await?;
    let blacklist = crate::blacklist::for_viewer(state, db, &page.current).await?;
    let blacklisted = if show_blacklisted {
        None
    } else {
        blacklist
            .as_ref()
            .and_then(|list| list.matching(post.rating, &post.tag_ids).map(str::to_owned))
    };
    let similar = similar_context(page, &asset, blacklist.as_ref()).await?;
    let deleted = if post.status == PostStatus::Deleted {
        crate::moderation::deletion(db, id).await?.map(|entry| {
            context! {
                by => entry.actor_name,
                reason => entry.reason,
                when => crate::dates::day(entry.created_at),
            }
        })
    } else {
        None
    };
    let flag_history = if page.current.can(Permission::ApprovePosts) {
        moekura_db::flags::for_post(db, id)
            .await?
            .into_iter()
            .map(|f| {
                context! {
                    by => f.creator_name,
                    reason => f.reason,
                    status => f.status,
                    when => crate::dates::day(f.created_at),
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let disapprovals: Vec<Value> =
        if page.current.can(Permission::ApprovePosts) && post.status == PostStatus::Pending {
            crate::moderation::disapprovals(db, &[id])
                .await?
                .into_iter()
                .map(|(_, d)| d)
                .collect()
        } else {
            Vec::new()
        };
    let appeal = crate::moderation::appeal_context(state, &page.current, &post).await?;
    let locks: Vec<Value> = moekura_core::posts::PostLock::ALL
        .iter()
        .map(|l| context! { name => l.as_str(), on => post.is_locked(*l) })
        .collect();
    let moderate = context! {
        locks => locks,
        locked => post.locks.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
        can_lock => page.current.can(Permission::LockPosts) && !limited,
        disapprovals => disapprovals,
        appeal => appeal,
        can_flag => page.current.is_logged_in()
            && page.current.can(Permission::Flag)
            && matches!(post.status, PostStatus::Active | PostStatus::Flagged),
        flags => flag_history,
        can_review => page.current.can(Permission::ApprovePosts) && post.status == PostStatus::Pending,
        can_delete => page.current.can(Permission::DeletePosts) && post.status != PostStatus::Deleted,
        can_restore => page.current.can(Permission::DeletePosts) && post.status == PostStatus::Deleted,
        can_purge => page.current.can(Permission::PurgePosts) && post.status == PostStatus::Deleted,
    };
    let show_url = {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if !search.is_empty() {
            query.append_pair("q", search);
        }
        query.append_pair("blacklist", "off");
        url_value(&format!("/posts/{id}?{}", query.finish()))
    };
    let (favorited, vote) = match me {
        Some(user) => (
            moekura_db::favorites::exists(db, user, id).await?,
            moekura_db::favorites::vote_of(db, user, id).await?,
        ),
        None => (false, 0),
    };
    let reactions = context! {
        score => post.score,
        fav_count => post.fav_count,
        favorited => favorited,
        vote => vote,
        can_favorite => me.is_some() && page.current.can(Permission::Favorite) && !limited,
        // Not on their own post, unless to take back a vote from before.
        can_vote => me.is_some() && page.current.can(Permission::Vote) && !limited
            && (post.uploader_id != me || vote != 0),
        // Keeps the search across the form's redirect.
        query => (!search.is_empty()).then(|| url_value(&format!(
            "?{}",
            url::form_urlencoded::Serializer::new(String::new()).append_pair("q", search).finish()
        ))),
    };
    let uploader = match post.uploader_id {
        Some(user_id) => users::by_id(db, user_id).await?.map(|u| u.name),
        None => None,
    };

    let url_of = |key: &str| file_url(state, key);
    let variant = |kind: &str| variants.iter().find(|v: &&Variant| v.kind == kind);
    let original = url_of(&asset.storage_key);
    let video = is_video(&asset.media_type);
    let animated = asset.frames > 1;
    // What the player plays: an ugoira's video, once made.
    let play = if asset.media_type == "ugoira" {
        variant("video").and_then(|v| url_of(&v.storage_key))
    } else {
        original.clone()
    };
    // Stills show the resized sample when there is one, unless the viewer
    // wants the original; animations and videos always use the original.
    let sample = variant("sample").filter(|_| !video && !animated);
    let show_original = extra.original.unwrap_or_else(|| {
        page.current
            .user
            .as_ref()
            .is_some_and(|u| UserSettings::from_json(&u.settings).original_images)
    });
    let display = match sample {
        Some(sample) if !show_original => url_of(&sample.storage_key),
        _ => original.clone(),
    };
    // Switches between the two without scripts; with them, in place.
    let resized = sample.map(|sample| {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if from_search {
            query.append_pair("q", search);
        }
        query.append_pair("original", if show_original { "0" } else { "1" });
        context! {
            percent => (i64::from(sample.width) * 100 / i64::from(asset.width.max(1))).max(1),
            sample => url_of(&sample.storage_key),
            showing_original => show_original,
            toggle_url => url_value(&format!("/posts/{id}?{}", query.finish())),
        }
    });
    let poster = variant("poster").and_then(|v| url_of(&v.storage_key));
    // Notes go on stills and animations, not videos.
    let notes = if video {
        Vec::new()
    } else {
        crate::notes::note_contexts(&moekura_db::notes::for_post(db, id, false).await?)
    };
    let created = post.created_at.format(&Rfc3339).unwrap_or_default();

    let file = context! {
        original => original,
        display => display,
        play => play,
        ugoira => asset.media_type == "ugoira",
        resized => resized,
        poster => poster,
        video => video,
        animated => animated,
        width => asset.width,
        height => asset.height,
        size => human_size(asset.file_size),
        media_type => asset.media_type.to_uppercase(),
        duration => asset.duration_ms.map(duration),
        has_audio => asset.has_audio,
    };
    let post_context = context! {
        id => post.id,
        rating => post.rating.label(),
        rating_code => post.rating.code(),
        status => post.status.as_str(),
        source => post.source,
        // Only web links become links; anything else (javascript:, data:)
        // is shown as text.
        source_link => is_web_url(&post.source),
        description => post.description,
        has_notes => post.last_noted_at.is_some(),
        embedded_notes => post.has_embedded_notes,
        created => crate::dates::day(post.created_at),
        created_iso => created,
    };
    let bound = |lock| post.is_locked(lock) && !page.current.can(Permission::LockPosts);
    let tags_locked = bound(moekura_core::posts::PostLock::Tags);
    let rating_locked = bound(moekura_core::posts::PostLock::Rating);
    let edit = (page.current.can(Permission::EditPosts) && !limited).then(|| match &failed {
        Some(failed) => context! {
            tags => failed.form.tags,
            old_tags => failed.form.old_tags,
            rating => failed.form.rating,
            source => failed.form.source,
            description => failed.form.description,
            parent => failed.form.parent,
            error => failed.error,
        },
        None => context! {
            tags => tag_string,
            old_tags => tag_string,
            rating => post.rating.code(),
            source => post.source,
            description => post.description,
            parent => post.parent_id.map(|p| p.to_string()).unwrap_or_default(),
        },
    });
    let edit = edit.map(|fields| {
        context! {
            ..fields,
            ..context! { tags_locked => tags_locked, rating_locked => rating_locked }
        }
    });
    let tag_warnings = match (&edit, extra.check) {
        (Some(_), Some(kept)) if failed.is_none() => {
            crate::tag_warnings::warnings(&post, &post_tags, &categories, kept)
        }
        _ => Vec::new(),
    };
    let copy_tags = if edit.is_some() && !tags_locked {
        copy_sources(page, &post).await?
    } else {
        Vec::new()
    };
    let suggestions = if edit.is_some() {
        crate::suggestions::for_edit_form(state, db, &post, &categories).await?
    } else {
        None
    };
    let ratings: Vec<Value> = Rating::ALL
        .iter()
        .map(|r| context! { code => r.code(), label => r.label() })
        .collect();
    let commentary = crate::commentary::for_post(db, id, edit.is_some()).await?;
    let comments =
        crate::comments::thread(state, &page.current, &post, extra.comment.as_ref()).await?;
    let pools = crate::pools::for_post(
        state,
        &page.current,
        &post,
        extra.pool.filter(|_| !from_search),
    )
    .await?;
    let favorite_groups = crate::favorite_groups::for_post(state, &page.current, post.id).await?;
    // Link previews only for posts visitors may see.
    let preview = if crate::previews::visitors(state).allows(&post) {
        let description = if post.description.is_empty() {
            tag_string.replace('_', " ")
        } else {
            post.description.clone()
        };
        let image = crate::previews::post_image(state, db, id, post.rating).await?;
        crate::previews::meta(
            state,
            &format!("/posts/{id}"),
            &crate::previews::post_title(id),
            &description,
            image,
            crate::previews::post_video(state, &asset, post.rating),
            true,
        )
    } else {
        None
    };
    let comment_refused = extra.comment.as_ref().is_some_and(|c| c.error.is_some());
    let status = if failed.is_some() || comment_refused {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    Ok(page.render_with_status(
        status,
        "post.html",
        context! {
            commentary => commentary,
            post => post_context,
            file => file,
            uploader => uploader,
            tag_groups => tag_groups,
            family => family,
            similar => similar,
            copy_tags => copy_tags,
            tag_warnings => tag_warnings,
            deleted => deleted,
            moderate => moderate,
            blacklisted => blacklisted.map(|rule| context! { rule => rule, show_url => show_url }),
            reactions => reactions,
            comments => comments,
            notes => notes,
            preview => preview,
            can_replace => page.current.can(Permission::ReplacePosts),
            can_edit_notes => !video
                && page.current.is_logged_in()
                && page.current.can(Permission::EditNotes)
                && post.status != PostStatus::Deleted,
            pools => pools,
            favorite_groups => favorite_groups,
            edit => edit,
            suggestions => suggestions,
            ratings => ratings,
            search => context! {
                tags => search,
                back_url => from_search.then(|| Value::from_safe_string(search_url(search))),
                // Stepping works where keyset pages do: id order.
                steps => (from_search && SearchQuery::parse(search).is_ok_and(|q| {
                    matches!(q.order, None | Some(Order::IdDesc | Order::IdAsc))
                }))
                .then(|| {
                    let q = url::form_urlencoded::Serializer::new(String::new())
                        .append_pair("q", search)
                        .finish();
                    context! {
                        previous => url_value(&format!("/posts/{id}/prev?{q}")),
                        next => url_value(&format!("/posts/{id}/next?{q}")),
                    }
                }),
            },
            processing => asset.processed_at.is_none(),
        },
    ))
}

/// Posts that look like this one, as thumbnails, leaving out those the
/// viewer can't see or has blacklisted.
async fn similar_context(
    page: &Page,
    asset: &media::Asset,
    blacklist: Option<&crate::blacklist::Active>,
) -> Result<Vec<Value>, AppError> {
    let Some(hash) = asset.phash else {
        return Ok(Vec::new());
    };
    let state = page.state();
    let db = state.db.primary();
    let found = media::similar(
        db,
        hash as u64,
        media::SIMILAR_MAX_DISTANCE,
        Some(asset.post_id),
        SIMILAR_SHOWN,
    )
    .await?;
    let ids: Vec<i64> = found.iter().map(|s| s.post_id).collect();
    let ids = posts::visible_ids(db, &ids, &visibility(&page.current)).await?;
    let thumbs = Thumbs::for_viewer(state, &page.current);
    let cards = posts::cards(db, &ids, thumbs.kinds()).await?;
    let labels = card_labels(db, &cards).await?;
    Ok(cards
        .iter()
        .filter(|card| {
            let rating = card.rating.parse().unwrap_or(Rating::Explicit);
            blacklist.is_none_or(|list| list.matching(rating, &card.tag_ids).is_none())
        })
        .map(|card| card_context(state, card, &labels, thumbs.size, None))
        .collect())
}

/// Similar posts listed on a post page.
const SIMILAR_SHOWN: i64 = 12;

/// Most children offered to copy tags from.
const COPY_SOURCES: usize = 10;

/// The posts the edit form offers to copy tags from: the parent and the
/// post's children, with their tags.
async fn copy_sources(page: &Page, post: &Post) -> Result<Vec<Value>, AppError> {
    let db = page.state().db.primary();
    let visible = visibility(&page.current);
    let mut ids: Vec<i64> = post.parent_id.into_iter().collect();
    ids.extend(
        posts::family(db, post.id, &visible)
            .await?
            .into_iter()
            .filter(|&id| id != post.id)
            .take(COPY_SOURCES),
    );
    let mut sources = Vec::new();
    for other in posts::by_ids(db, &ids).await? {
        if !visible.allows(&other) {
            continue;
        }
        let mut names: Vec<String> = tags::by_ids(db, &other.tag_ids)
            .await?
            .into_iter()
            .map(|t| t.name)
            .collect();
        names.sort();
        sources.push(context! {
            id => other.id,
            parent => Some(other.id) == post.parent_id,
            tags => names.join(" "),
        });
    }
    Ok(sources)
}

/// The parent/children bar: the post's parent and its other children, or
/// the post's own children. `None` when the post has no family.
async fn family_context(page: &Page, post: &Post) -> Result<Option<Value>, AppError> {
    let state = page.state();
    let db = state.db.primary();
    let root = post.parent_id.unwrap_or(post.id);
    let ids = posts::family(db, root, &visibility(&page.current)).await?;
    if ids.len() < 2 {
        return Ok(None);
    }
    let thumbs = Thumbs::for_viewer(state, &page.current);
    let cards = posts::cards(db, &ids, thumbs.kinds()).await?;
    let labels = card_labels(db, &cards).await?;
    Ok(Some(context! {
        is_child => post.parent_id.is_some(),
        root => root,
        cards => cards
            .iter()
            .map(|card| context! {
                ..card_context(state, card, &labels, thumbs.size, None),
                ..context! { current => card.id == post.id }
            })
            .collect::<Vec<_>>(),
    }))
}

fn is_web_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

pub(crate) fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes.max(0) as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[test]
    fn fits_into_boxes() {
        assert_eq!(fit(800, 600, 250), (250, 188));
        assert_eq!(fit(600, 800, 250), (188, 250));
        assert_eq!(fit(100, 50, 250), (100, 50));
    }

    #[test]
    fn counts_read_well() {
        assert_eq!(count_text(Count::Exact(1)), "1 post");
        assert_eq!(count_text(Count::Exact(12_345)), "12,345 posts");
        assert_eq!(count_text(Count::About(1_000_000)), "about 1,000,000 posts");
        assert_eq!(count_text(Count::AtLeast(10_000)), "10,000+ posts");
        assert_eq!(thousands(-1234), "-1,234");
        assert_eq!(thousands(999), "999");
    }

    fn pager(page: PageRef, count: Count, full: bool) -> Pager<'static> {
        Pager {
            query: "cat",
            page,
            per_page: 10,
            max_page: 5,
            keyset: true,
            descending: true,
            count,
            first: Some(90),
            last: Some(81),
            full,
            show_blacklisted: false,
        }
    }

    #[test]
    fn pager_links() {
        let links = |page, count, full| {
            let value = pager(page, count, full).context();
            let get = |key: &str| {
                let v = value.get_attr(key).unwrap();
                (!v.is_none()).then(|| v.to_string())
            };
            let numbers: Vec<String> = value
                .get_attr("numbers")
                .unwrap()
                .try_iter()
                .unwrap()
                .map(|n| match n.get_attr("number").unwrap() {
                    v if v.is_undefined() => "…".to_owned(),
                    v if n.get_attr("current").unwrap().is_true() => format!("[{v}]"),
                    v => v.to_string(),
                })
                .collect();
            (get("previous"), get("next"), numbers.join(" "))
        };
        // 35 posts at 10 a page: 4 pages.
        let (previous, next, numbers) = links(PageRef::Number(1), Count::Exact(35), true);
        assert_eq!(previous, None);
        assert_eq!(next.as_deref(), Some("/posts?tags=cat&amp;page=2"));
        assert_eq!(numbers, "[1] 2 3 4");
        let (previous, next, _) = links(PageRef::Number(4), Count::Exact(35), false);
        assert_eq!(previous.as_deref(), Some("/posts?tags=cat&amp;page=3"));
        assert_eq!(next, None);
        // Beyond the numbered pages, "next" continues by id.
        let (_, next, numbers) = links(PageRef::Number(5), Count::AtLeast(100), true);
        assert_eq!(next.as_deref(), Some("/posts?tags=cat&amp;page=b81"));
        assert_eq!(numbers, "1 … 3 4 [5]");
        let (previous, next, numbers) = links(PageRef::Before(100), Count::AtLeast(100), true);
        assert_eq!(previous.as_deref(), Some("/posts?tags=cat&amp;page=a90"));
        assert_eq!(next.as_deref(), Some("/posts?tags=cat&amp;page=b81"));
        assert_eq!(numbers, "");
        // Large counts show the last page too.
        let (_, _, numbers) = links(PageRef::Number(1), Count::About(1_000), true);
        assert_eq!(numbers, "[1] 2 3 … 5");
    }

    #[test]
    fn sizes_and_links() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(54_043), "52.8 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
        assert!(is_web_url("https://example.com/a"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("not a url"));
    }

    async fn app(pool: &PgPool) -> (TestApp, AppState) {
        let state = test_state(pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = routes().merge(crate::upload::routes(max));
        (TestApp::new(state.clone(), routes), state)
    }

    /// Uploads through the real endpoint and returns the post id.
    async fn upload(
        app: &TestApp,
        session: &str,
        png: &[u8],
        extra: &[(&'static str, &str)],
    ) -> i64 {
        let mut fields = vec![("rating", "s".to_owned())];
        fields.extend(extra.iter().map(|(k, v)| (*k, (*v).to_owned())));
        let response = app
            .post_multipart("/upload", Some(session), &fields, Some(("a.png", png)))
            .await;
        response
            .location
            .unwrap_or_else(|| panic!("upload failed: {}", response.body))
            .strip_prefix("/posts/")
            .unwrap()
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn grid_lists_posts_newest_first(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let older = upload(&app, &session, &fixture::png(40, 30), &[]).await;
        let newer = upload(&app, &session, &fixture::png(30, 40), &[]).await;

        let page = app.get("/", None).await;
        assert_eq!(page.status, StatusCode::OK);
        let first = page
            .body
            .find(&format!("href=\"/posts/{newer}?q="))
            .unwrap();
        let second = page
            .body
            .find(&format!("href=\"/posts/{older}?q="))
            .unwrap();
        assert!(first < second);
        // Not processed yet: placeholders, not broken images.
        assert!(
            page.body.contains("class=\"thumb pending-media\""),
            "{}",
            page.body
        );

        let page = app.get(&format!("/?page=b{newer}"), None).await;
        assert!(!page.body.contains(&format!("href=\"/posts/{newer}?q=")));
        assert!(page.body.contains(&format!("href=\"/posts/{older}?q=")));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn post_page_shows_the_file_and_details(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(
            &app,
            &session,
            &fixture::png(64, 48),
            &[("source", "javascript:alert(1)")],
        )
        .await;

        let page = app.get(&format!("/posts/{id}"), None).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        assert!(page.body.contains("<img"), "{}", page.body);
        assert!(page.body.contains("/data/original/"));
        assert!(page.body.contains("64×48"));
        assert!(page.body.contains("Sensitive"));
        assert!(page.body.contains("alice"));
        assert!(
            page.body.contains("javascript:alert(1)"),
            "source still shown"
        );
        assert!(
            !page.body.contains("href=\"javascript:"),
            "but not as a link"
        );
        // A source on a site we know shows its icon and name.
        sqlx::query("UPDATE posts SET source = 'https://www.pixiv.net/artworks/1' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let page = app.get(&format!("/posts/{id}"), None).await;
        assert!(
            page.body.contains("<title>Pixiv</title>") && page.body.contains("#pixiv\""),
            "{}",
            page.body
        );

        assert_eq!(
            app.get(&format!("/posts/{}", id + 100), None).await.status,
            StatusCode::NOT_FOUND
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn post_page_groups_tags_by_category(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(
            &app,
            &session,
            &fixture::png(20, 20),
            &[("tags", "zebra apple artist:someone c++")],
        )
        .await;
        let body = app.get(&format!("/posts/{id}"), None).await.body;
        let position = |needle: &str| {
            body.find(needle)
                .unwrap_or_else(|| panic!("{needle} missing from {body}"))
        };
        assert!(position("<h2>Artist</h2>") < position("<h2>General</h2>"));
        // Alphabetical within a group.
        assert!(position(">apple<") < position(">zebra<"));
        assert!(body.contains("class=\"tag tag-artist\" href=\"/posts?tags=someone\""));
        assert!(body.contains("href=\"/posts?tags=c%2B%2B\""), "{body}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn searching_by_tags(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let session = session_for(&pool, "alice", SystemRole::Member).await;
        let cat = upload(
            &app,
            &session,
            &fixture::png(20, 20),
            &[("tags", "cat cute")],
        )
        .await;
        let dog = upload(
            &app,
            &session,
            &fixture::png(24, 20),
            &[("tags", "dog cute")],
        )
        .await;

        let page = app.get("/posts?tags=Cute+-dog", None).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        assert!(
            page.body
                .contains(&format!("href=\"/posts/{cat}?q=cute+-dog\"")),
            "{}",
            page.body
        );
        assert!(!page.body.contains(&format!("/posts/{dog}")));
        assert!(page.body.contains("1 post"));
        // The normalised query goes back into the search box.
        assert!(page.body.contains("value=\"cute -dog\""), "{}", page.body);
        // The sidebar lists the page's tags with links to refine.
        assert!(
            page.body.contains("href=\"/posts?tags=cute+-dog+cat\""),
            "{}",
            page.body
        );
        assert!(page.body.contains("href=\"/posts?tags=cute+-dog+-cat\""));

        let post = app.get(&format!("/posts/{cat}?q=cute+-dog"), None).await;
        assert!(
            post.body.contains("href=\"/posts?tags=cute+-dog\""),
            "{}",
            post.body
        );

        let bad = app.get("/posts?tags=rating:x", None).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert!(bad.body.contains("expected ratings"), "{}", bad.body);
        let nothing = app.get("/posts?tags=nonexistent", None).await;
        assert!(nothing.body.contains("Nothing found"), "{}", nothing.body);

        // Hot posts have their own link.
        let hot = app.get("/posts?tags=order%3Arank", None).await.body;
        assert!(
            hot.contains("href=\"/posts?tags=order%3Arank\" aria-current=\"page\">Hot<"),
            "{hot}"
        );
        assert!(!page.body.contains("aria-current=\"page\">Hot<"));

        // Groups and `or`.
        let page = app.get("/posts?tags=(cat+cute)+or+(dog+-cute)", None).await;
        assert!(
            page.body.contains(&format!("/posts/{cat}?")),
            "{}",
            page.body
        );
        assert!(!page.body.contains(&format!("/posts/{dog}?")));
        assert!(page.body.contains("value=\"((cat cute) or (dog -cute))\""));
        let bad = app.get("/posts?tags=(cat+or", None).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
        assert!(bad.body.contains("is missing its"), "{}", bad.body);
        // No "without the last term" that would break the group.
        // Likely typos get replacements.
        let typo = app.get("/posts?tags=rating:g+cuet", None).await.body;
        assert!(
            typo.contains(
                "Did you mean <a href=\"/posts?tags=cute+rating%3Ag\"><strong>cute</strong></a>"
            ) && typo.contains(" instead of cuet?"),
            "{typo}"
        );
        let typo = app.get("/posts?tags=dgo", None).await.body;
        assert!(typo.contains("<strong>dog</strong></a>?"), "{typo}");
        assert!(!typo.contains("Look for tags starting with"));
        let nothing = app.get("/posts?tags=dog+(cat+or+rating:e)", None).await;
        assert!(nothing.body.contains("Nothing found"), "{}", nothing.body);
        assert!(
            !nothing.body.contains("without the last term"),
            "{}",
            nothing.body
        );
        assert_eq!(
            app.get("/posts?page=x", None).await.status,
            StatusCode::BAD_REQUEST
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn visitors_see_only_allowed_ratings(pool: PgPool) {
        moekura_db::settings::set(&pool, "visitor_ratings", serde_json::json!(["g", "s"]))
            .await
            .unwrap();
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let explicit = upload(&app, &alice, &fixture::png(20, 20), &[("rating", "e")]).await;
        let general = upload(&app, &alice, &fixture::png(24, 20), &[("rating", "g")]).await;

        let grid = app.get("/", None).await.body;
        assert!(grid.contains(&format!("/posts/{general}?")), "{grid}");
        assert!(!grid.contains(&format!("/posts/{explicit}")), "{grid}");
        assert!(grid.contains("1 post"), "{grid}");
        let searched = app.get("/posts?tags=rating:e", None).await.body;
        assert!(searched.contains("Nothing found"), "{searched}");
        assert_eq!(
            app.get(&format!("/posts/{explicit}"), None).await.status,
            StatusCode::NOT_FOUND
        );
        // Members see everything.
        let grid = app.get("/", Some(&alice)).await.body;
        assert!(grid.contains(&format!("/posts/{explicit}?")), "{grid}");
        assert_eq!(
            app.get(&format!("/posts/{explicit}"), Some(&alice))
                .await
                .status,
            StatusCode::OK
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn blacklists_hide_posts(pool: PgPool) {
        moekura_db::settings::set(
            &pool,
            "default_blacklist",
            serde_json::json!("rating:e\nkitty"),
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('alias', 'kitty', 'cat', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let fine = upload(&app, &alice, &fixture::png(28, 20), &[]).await;
        let explicit = upload(&app, &alice, &fixture::png(20, 20), &[("rating", "e")]).await;
        let cat = upload(&app, &alice, &fixture::png(24, 20), &[("tags", "cat")]).await;

        let grid = app.get("/", None).await.body;
        assert!(grid.contains(&format!("href=\"/posts/{fine}?q=")));
        assert!(!grid.contains(&format!("/posts/{explicit}")), "{grid}");
        assert!(
            !grid.contains(&format!("/posts/{cat}\"")),
            "aliases count too"
        );
        assert!(grid.contains("2 hidden by your blacklist"), "{grid}");
        assert!(grid.contains("href=\"/posts?blacklist=off\""));
        let all = app.get("/posts?blacklist=off", None).await.body;
        assert!(all.contains(&format!("/posts/{explicit}")));

        // The search leaves them out, so pages stay full and the count
        // and pages are right.
        let first = app.get("/posts?tags=limit%3A1", None).await.body;
        assert!(first.contains(&format!("/posts/{fine}?")), "{first}");
        assert!(first.contains("result-count\">1 post"), "{first}");
        assert!(!first.contains("page=2"), "{first}");
        let unfiltered = app
            .get("/posts?tags=limit%3A1&blacklist=off", None)
            .await
            .body;
        assert!(unfiltered.contains(&format!("/posts/{cat}?")));
        assert!(
            unfiltered.contains("page=2&amp;blacklist=off"),
            "{unfiltered}"
        );
        // Stepping through results skips them too.
        let next = app.get(&format!("/posts/{cat}/next?q="), None).await;
        assert_eq!(
            next.location.as_deref(),
            Some(format!("/posts/{fine}?q=").as_str())
        );

        let post = app.get(&format!("/posts/{explicit}"), None).await.body;
        assert!(post.contains("matches your blacklist"), "{post}");
        assert!(!post.contains("<img src=\"/data/"));
        let shown = app
            .get(&format!("/posts/{explicit}?blacklist=off"), None)
            .await
            .body;
        assert!(!shown.contains("matches your blacklist"));
    }

    /// Stores `settings` for the user called `name`.
    async fn set_user_settings(pool: &PgPool, name: &str, settings: serde_json::Value) {
        sqlx::query("UPDATE users SET settings = $2 WHERE name = $1")
            .bind(name)
            .bind(settings)
            .execute(pool)
            .await
            .unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn display_settings_apply(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let general = upload(&app, &alice, &fixture::png(20, 20), &[("rating", "g")]).await;
        let explicit = upload(&app, &alice, &fixture::png(24, 20), &[("rating", "e")]).await;

        // Safe mode: general posts only, in searches and on post pages.
        set_user_settings(&pool, "alice", serde_json::json!({ "safe_mode": true })).await;
        let grid = app.get("/", Some(&alice)).await.body;
        assert!(grid.contains(&format!("/posts/{general}?")), "{grid}");
        assert!(!grid.contains(&format!("/posts/{explicit}?")), "{grid}");
        assert_eq!(
            app.get(&format!("/posts/{explicit}"), Some(&alice))
                .await
                .status,
            StatusCode::NOT_FOUND
        );

        // Blurred rather than left out, with larger thumbnails, no
        // comments, autocomplete or shortcuts.
        set_user_settings(
            &pool,
            "alice",
            serde_json::json!({
                "blacklist": "rating:e",
                "blur_blacklisted": true,
                "large_thumbnails": true,
                "hide_comments": true,
                "autocomplete": false,
                "shortcuts": false,
            }),
        )
        .await;
        let grid = app.get("/", Some(&alice)).await.body;
        assert!(grid.contains("is-blacklisted"), "{grid}");
        assert!(grid.contains(&format!("/posts/{explicit}?")), "{grid}");
        assert!(grid.contains("1 blurred by your blacklist"), "{grid}");
        assert!(
            grid.contains("data-thumbs=\"large\" data-autocomplete=\"off\" data-shortcuts=\"off\""),
            "{grid}"
        );
        assert!(!grid.contains("data-shortcuts-link"));
        let post = app
            .get(&format!("/posts/{general}"), Some(&alice))
            .await
            .body;
        assert!(post.contains("Your settings hide comments"), "{post}");
        assert!(!post.contains("id=\"new-comment\""));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn cards_show_status_family_and_length(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let parent = upload(&app, &alice, &fixture::png(20, 20), &[]).await;
        let child = upload(&app, &alice, &fixture::png(24, 20), &[]).await;
        sqlx::query("UPDATE posts SET parent_id = $1, status = 'flagged' WHERE id = $2")
            .bind(parent)
            .bind(child)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE media_assets SET media_type = 'mp4', duration_ms = 75400, has_audio = true
             WHERE post_id = $1",
        )
        .bind(child)
        .execute(&pool)
        .await
        .unwrap();

        let grid = app.get("/", Some(&alice)).await.body;
        assert!(
            grid.contains("class=\"post-card rating-s is-active has-children\""),
            "{grid}"
        );
        assert!(
            grid.contains("class=\"post-card rating-s is-flagged has-parent\""),
            "{grid}"
        );
        assert!(grid.contains("(flagged, has a parent)"), "{grid}");
        assert!(
            grid.contains("<span class=\"badge duration\">1:15<svg"),
            "{grid}"
        );
        assert!(grid.contains(", with sound"), "{grid}");
        assert!(!grid.contains(">video<"), "{grid}");

        // A deleted child no longer counts.
        sqlx::query("UPDATE posts SET status = 'deleted' WHERE id = $1")
            .bind(child)
            .execute(&pool)
            .await
            .unwrap();
        let grid = app.get("/", Some(&alice)).await.body;
        assert!(!grid.contains("has-children"), "{grid}");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(5_999), "0:05");
        assert_eq!(duration(75_400), "1:15");
        assert_eq!(duration(3_723_000), "1:02:03");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn deleted_posts_hidden_from_searches(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let kept = upload(&app, &alice, &fixture::png(20, 20), &[("tags", "cat")]).await;
        let gone = upload(&app, &alice, &fixture::png(24, 20), &[("tags", "cat")]).await;
        sqlx::query("UPDATE posts SET status = 'deleted' WHERE id = $1")
            .bind(gone)
            .execute(&pool)
            .await
            .unwrap();

        let grid = app.get("/posts?tags=cat", Some(&moderator)).await.body;
        assert!(grid.contains(&format!("/posts/{kept}?")), "{grid}");
        assert!(!grid.contains(&format!("/posts/{gone}?")), "{grid}");
        assert!(grid.contains("1 deleted post hidden"), "{grid}");
        assert!(
            grid.contains("href=\"/posts?tags=cat+status%3Aany\""),
            "{grid}"
        );
        // Not for those who couldn't see them anyway, nor with a status:.
        let member = app.get("/posts?tags=cat", Some(&alice)).await.body;
        assert!(!member.contains("deleted post"), "{member}");
        let any = app
            .get("/posts?tags=cat+status:any", Some(&moderator))
            .await
            .body;
        assert!(any.contains(&format!("/posts/{gone}?")), "{any}");
        assert!(!any.contains("deleted post hidden"));

        // Included by choice.
        set_user_settings(&pool, "mod", serde_json::json!({ "show_deleted": true })).await;
        let grid = app.get("/posts?tags=cat", Some(&moderator)).await.body;
        assert!(grid.contains(&format!("/posts/{gone}?")), "{grid}");
        assert!(!grid.contains("deleted post hidden"));
        set_user_settings(&pool, "alice", serde_json::json!({ "show_deleted": true })).await;
        let member = app.get("/posts?tags=cat", Some(&alice)).await.body;
        assert!(!member.contains(&format!("/posts/{gone}?")), "{member}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn resized_samples_say_so(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, &fixture::png(40, 20), &[]).await;
        let page = app.get(&format!("/posts/{id}"), None).await.body;
        assert!(!page.contains("data-resized"), "no sample, no notice");
        sqlx::query(
            "INSERT INTO media_variants (asset_id, kind, format, width, height, file_size, storage_key)
             SELECT id, 'sample', 'webp', 10, 5, 1, 'samples/ab/cd/abcd.webp'
             FROM media_assets WHERE post_id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        let page = app.get(&format!("/posts/{id}?q=x"), None).await.body;
        assert!(page.contains("Resized to 25% of the original."), "{page}");
        assert!(
            page.contains("<img src=\"/data/samples/ab/cd/abcd.webp\""),
            "{page}"
        );
        assert!(
            page.contains(&format!("href=\"/posts/{id}?q=x&amp;original=1\"")),
            "{page}"
        );
        let original = app.get(&format!("/posts/{id}?original=1"), None).await.body;
        assert!(original.contains("Showing the original."), "{original}");
        assert!(!original.contains("<img src=\"/data/samples/"));

        // Or always, by choice, until asked for the sample.
        set_user_settings(
            &pool,
            "alice",
            serde_json::json!({ "original_images": true }),
        )
        .await;
        let chosen = app.get(&format!("/posts/{id}"), Some(&alice)).await.body;
        assert!(chosen.contains("Showing the original."), "{chosen}");
        assert!(chosen.contains(&format!("href=\"/posts/{id}?original=0\"")));
        let sample = app
            .get(&format!("/posts/{id}?original=0"), Some(&alice))
            .await
            .body;
        assert!(sample.contains("Resized to 25%"), "{sample}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn dates_are_in_the_viewers_time_zone(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, &fixture::png(20, 20), &[]).await;
        sqlx::query("UPDATE posts SET created_at = '2026-01-01 23:30Z' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let utc = app.get(&format!("/posts/{id}"), Some(&alice)).await.body;
        assert!(utc.contains(">2026-01-01<"), "{utc}");
        set_user_settings(
            &pool,
            "alice",
            serde_json::json!({ "time_zone": "Asia/Tokyo" }),
        )
        .await;
        let tokyo = app.get(&format!("/posts/{id}"), Some(&alice)).await.body;
        assert!(tokyo.contains(">2026-01-02<"), "{tokyo}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn post_pages_list_similar_posts(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let a = upload(&app, &alice, &fixture::png(20, 20), &[]).await;
        let b = upload(&app, &alice, &fixture::png(24, 20), &[]).await;
        let c = upload(&app, &alice, &fixture::png(28, 20), &[("rating", "e")]).await;
        // By an artist banned since, and a child of `a`.
        let d = upload(
            &app,
            &alice,
            &fixture::png(32, 20),
            &[("tags", "bad_artist")],
        )
        .await;
        sqlx::query("INSERT INTO artists (name, is_banned) VALUES ('bad_artist', true)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE posts SET parent_id = $1 WHERE id = $2")
            .bind(a)
            .bind(d)
            .execute(&pool)
            .await
            .unwrap();
        // As if processing found them nearly identical.
        for (post, hash) in [(a, 0x1234_i64), (b, 0x1235), (c, 0x1237), (d, 0x1236)] {
            sqlx::query(
                "UPDATE media_assets SET phash = $2, phash_0 = ($2 >> 48)::int2,
                     phash_1 = (($2 >> 32) & 65535)::int2, phash_2 = (($2 >> 16) & 65535)::int2,
                     phash_3 = ($2 & 65535)::int2, processed_at = now()
                 WHERE post_id = $1",
            )
            .bind(post)
            .bind(hash)
            .execute(&pool)
            .await
            .unwrap();
        }
        moekura_db::settings::set(&pool, "default_blacklist", serde_json::json!("rating:e"))
            .await
            .unwrap();
        let (app, _) = super::tests::app(&pool).await;
        let page = app.get(&format!("/posts/{a}"), None).await.body;
        assert!(page.contains("Similar posts"), "{page}");
        assert!(page.contains(&format!("href=\"/posts/{b}\"")), "{page}");
        assert!(
            !page.contains(&format!("href=\"/posts/{c}\"")),
            "blacklisted"
        );
        assert!(page.contains(&format!("href=\"/posts?tags=similar%3A{a}\"")));
        // Neither among similar posts nor in the family bar.
        assert!(!page.contains(&format!("href=\"/posts/{d}\"")), "{page}");
        assert!(!page.contains("class=\"family\""), "{page}");
        let admin = session_for(&pool, "boss", SystemRole::Admin).await;
        let staff = app.get(&format!("/posts/{a}"), Some(&admin)).await.body;
        assert!(staff.contains(&format!("href=\"/posts/{d}\"")), "{staff}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn stepping_through_a_search(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let first = upload(&app, &alice, &fixture::png(20, 20), &[("tags", "x")]).await;
        upload(&app, &alice, &fixture::png(24, 20), &[]).await;
        let second = upload(&app, &alice, &fixture::png(28, 20), &[("tags", "x")]).await;
        let third = upload(&app, &alice, &fixture::png(32, 20), &[("tags", "x")]).await;
        let step = |from: i64, way: &str, q: &str| {
            let url = format!("/posts/{from}/{way}?q={q}");
            let app = &app;
            async move { app.get(&url, None).await.location.unwrap() }
        };
        // Newest first: "next" goes to older posts, skipping non-matches.
        assert_eq!(
            step(third, "next", "x").await,
            format!("/posts/{second}?q=x")
        );
        assert_eq!(
            step(second, "next", "x").await,
            format!("/posts/{first}?q=x")
        );
        assert_eq!(
            step(second, "prev", "x").await,
            format!("/posts/{third}?q=x")
        );
        // Past either end: back to the search.
        assert_eq!(step(first, "next", "x").await, "/posts?tags=x");
        assert_eq!(step(third, "prev", "x").await, "/posts?tags=x");
        // Oldest first flips the direction.
        assert_eq!(
            step(first, "next", "x+order%3Aid_asc").await,
            format!("/posts/{second}?q=x+order%3Aid_asc")
        );

        let page = app.get(&format!("/posts/{second}?q=x"), None).await.body;
        assert!(
            page.contains(&format!("href=\"/posts/{second}/next?q=x\" rel=\"next\"")),
            "{page}"
        );
        let from_home = app.get(&format!("/posts/{second}?q="), None).await.body;
        assert!(from_home.contains("All posts"), "{from_home}");
        let by_score = app
            .get(&format!("/posts/{second}?q=order%3Ascore"), None)
            .await
            .body;
        assert!(!by_score.contains("rel=\"next\""), "only id order steps");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn pending_posts_are_hidden_from_others(pool: PgPool) {
        moekura_db::settings::set(&pool, "upload_approval", serde_json::json!(true))
            .await
            .unwrap();
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let janitor = session_for(&pool, "jan", SystemRole::Janitor).await;
        let id = upload(&app, &alice, &fixture::png(20, 20), &[]).await;
        let path = format!("/posts/{id}");

        assert_eq!(app.get(&path, None).await.status, StatusCode::NOT_FOUND);
        assert_eq!(app.get(&path, Some(&alice)).await.status, StatusCode::OK);
        assert_eq!(app.get(&path, Some(&janitor)).await.status, StatusCode::OK);
        assert!(!app.get("/", None).await.body.contains(&path));
        assert!(app.get("/", Some(&alice)).await.body.contains(&path));
    }
}
