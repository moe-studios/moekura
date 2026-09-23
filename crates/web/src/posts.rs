//! The post grid (front page) and single post pages.

use axum::Router;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use minijinja::{Value, context};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use uwuu_core::permissions::Permission;
use uwuu_core::posts::{PostStatus, Rating};
use uwuu_core::search::{Order, Query as SearchQuery};
use uwuu_core::user_settings::UserSettings;
use uwuu_db::media::{self, Variant};
use uwuu_db::posts::{self, Card, Post, Visibility};
use uwuu_db::search::{Count, PageRef, Plan, SearchError};
use uwuu_db::{tags, users};
use uwuu_storage::Key;

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
        statuses,
        viewer: current.user.as_ref().map(|u| u.id),
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

/// Search results; the front page is the empty search.
async fn index(page: Page, Query(params): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.db.read();
    // The user's page size, within the site's limit.
    let mut config = state.config.search.clone();
    if let Some(per_page) = page
        .current
        .user
        .as_ref()
        .and_then(|u| UserSettings::from_json(&u.settings).per_page)
    {
        config.per_page = per_page.min(config.max_per_page);
    }
    let config = &config;
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
    let plan = match Plan::resolve(db, &query, &visibility(&page.current), config).await {
        Ok(plan) => plan,
        Err(SearchError::Invalid(message)) => return Ok(failed(message)),
        Err(SearchError::Db(error)) => return Err(error.into()),
    };
    let (ids, count) = match (plan.ids(db, page_ref).await, plan.count(db).await) {
        (Ok(ids), Ok(count)) => (ids, count),
        (Err(SearchError::Invalid(message)), _) => return Ok(failed(message)),
        (Err(SearchError::Db(error)), _) | (_, Err(SearchError::Db(error))) => {
            return Err(error.into());
        }
        (_, Err(SearchError::Invalid(message))) => return Ok(failed(message)),
    };

    let sizes = &state.media.config().thumbnail_sizes;
    let box_size = sizes.first().copied().unwrap_or(250);
    let kinds = (
        format!("thumb-{box_size}"),
        format!("thumb-{}", sizes.get(1).copied().unwrap_or(box_size)),
    );
    let cards = posts::cards(db, &ids, (&kinds.0, &kinds.1)).await?;
    let normalized = query.to_string();
    // Even the empty search, so the post page can step through it.
    let post_query = Some(
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", &normalized)
            .finish(),
    );
    // Blacklisted posts are left out of the page entirely, with a count
    // and a link to show them.
    let show_all = params.blacklist == "off";
    let blacklist = crate::blacklist::for_viewer(state, db, &page.current).await?;
    let (shown, hidden): (Vec<&Card>, Vec<&Card>) = cards.iter().partition(|card| {
        show_all
            || blacklist.as_ref().is_none_or(|list| {
                let rating = card.rating.parse().unwrap_or(Rating::Explicit);
                list.matching(rating, &card.tag_ids).is_none()
            })
    });
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
    let blacklisted = context! {
        hidden => hidden.len(),
        show_url => (!hidden.is_empty()).then(|| blacklist_url(true)),
        hide_url => (show_all && blacklist.is_some()).then(|| blacklist_url(false)),
    };
    let card_values: Vec<Value> = shown
        .iter()
        .map(|card| card_context(state, card, box_size, post_query.as_deref()))
        .collect();

    let shown: Vec<Card> = shown.into_iter().cloned().collect();
    let sidebar = sidebar_tags(db, &shown, &normalized).await?;
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
    };
    Ok(page.render(
        "posts.html",
        context! {
            search => context! { tags => normalized },
            cards => card_values,
            blacklisted => blacklisted,
            count => count_text(count),
            sidebar => sidebar,
            pager => pager.context(),
        },
    ))
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
                include_url => with(tag.name.clone()),
                exclude_url => with(format!("-{}", tag.name)),
            }
        })
        .collect())
}

fn count_text(count: Count) -> String {
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
}

impl Pager<'_> {
    fn url(&self, page: &str) -> Value {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if !self.query.is_empty() {
            query.append_pair("tags", self.query);
        }
        query.append_pair("page", page);
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
    Key::parse(key).map(|k| Value::from_safe_string(state.storage.url(&k)))
}

/// A grid card. `post_query` (`q=…`) is added to the post link so the post
/// page can lead back to the search.
fn card_context(state: &AppState, card: &Card, box_size: u32, post_query: Option<&str>) -> Value {
    let url = |key: &Option<String>| key.as_deref().and_then(|k| file_url(state, k));
    let (width, height) = fit(card.width, card.height, box_size);
    let href = match post_query {
        Some(query) => format!("/posts/{}?{query}", card.id),
        None => format!("/posts/{}", card.id),
    };
    context! {
        id => card.id,
        href => Value::from_safe_string(href),
        thumb => url(&card.thumb),
        thumb_2x => url(&card.thumb_2x),
        width => width,
        height => height,
        rating => card.rating,
        pending => card.status == "pending",
        deleted => card.status == "deleted",
        video => matches!(card.media_type.as_str(), "mp4" | "webm"),
        animated => card.frames > 1,
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
    let db = state.db.read();
    let mut query = SearchQuery::parse(q).map_err(|e| AppError::BadRequest(e.to_string()))?;
    query.limit = Some(1);
    let plan =
        match Plan::resolve(db, &query, &visibility(&page.current), &state.config.search).await {
            Ok(plan) => plan,
            Err(SearchError::Invalid(message)) => return Err(AppError::BadRequest(message)),
            Err(SearchError::Db(error)) => return Err(error.into()),
        };
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
}

async fn show(
    page: Page,
    Path(id): Path<i64>,
    Query(params): Query<ShowQuery>,
) -> Result<Response, AppError> {
    render_post(
        &page,
        id,
        params.q.as_deref(),
        params.blacklist == "off",
        None,
    )
    .await
}

/// The edit form as submitted, shown again with an error.
pub(crate) struct FailedEdit<'a> {
    pub form: &'a crate::edit::EditForm,
    pub error: String,
}

/// The post page. `failed` refills the edit form after a rejected edit.
pub(crate) async fn render_post(
    page: &Page,
    id: i64,
    search: Option<&str>,
    show_blacklisted: bool,
    failed: Option<FailedEdit<'_>>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let from_search = search.is_some();
    let search = search.unwrap_or_default();
    let state = page.state();
    // The primary, so an uploader redirected here sees their post even if
    // a replica lags.
    let db = state.db.primary();
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !visibility(&page.current).allows(&post) {
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
                when => entry.created_at.date().to_string(),
            }
        })
    } else {
        None
    };
    let flag_history = if page.current.can(Permission::ApprovePosts) {
        uwuu_db::flags::for_post(db, id)
            .await?
            .into_iter()
            .map(|f| {
                context! {
                    by => f.creator_name,
                    reason => f.reason,
                    status => f.status,
                    when => f.created_at.date().to_string(),
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let moderate = context! {
        can_flag => page.current.is_logged_in()
            && page.current.can(Permission::Flag)
            && matches!(post.status, PostStatus::Active | PostStatus::Flagged),
        flags => flag_history,
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
    let me = page.current.user.as_ref().map(|u| u.id);
    let (favorited, vote) = match me {
        Some(user) => (
            uwuu_db::favorites::exists(db, user, id).await?,
            uwuu_db::favorites::vote_of(db, user, id).await?,
        ),
        None => (false, 0),
    };
    let reactions = context! {
        score => post.score,
        fav_count => post.fav_count,
        favorited => favorited,
        vote => vote,
        can_favorite => me.is_some() && page.current.can(Permission::Favorite),
        can_vote => me.is_some() && page.current.can(Permission::Vote),
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
    let video = matches!(asset.media_type.as_str(), "mp4" | "webm");
    let animated = asset.frames > 1;
    // Stills show the resized sample when there is one; animations and
    // videos always use the original.
    let display = match variant("sample") {
        Some(sample) if !video && !animated => url_of(&sample.storage_key),
        _ => original.clone(),
    };
    let poster = variant("poster").and_then(|v| url_of(&v.storage_key));
    let created = post.created_at.format(&Rfc3339).unwrap_or_default();

    let file = context! {
        original => original,
        display => display,
        poster => poster,
        video => video,
        animated => animated,
        width => asset.width,
        height => asset.height,
        size => human_size(asset.file_size),
        media_type => asset.media_type.to_uppercase(),
        duration => asset.duration_ms.map(|ms| format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)),
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
        created => created.get(..10).unwrap_or_default(),
        created_iso => created,
    };
    let edit = page
        .current
        .can(Permission::EditPosts)
        .then(|| match &failed {
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
    let ratings: Vec<Value> = Rating::ALL
        .iter()
        .map(|r| context! { code => r.code(), label => r.label() })
        .collect();
    let status = if failed.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    Ok(page.render_with_status(
        status,
        "post.html",
        context! {
            post => post_context,
            file => file,
            uploader => uploader,
            tag_groups => tag_groups,
            family => family,
            similar => similar,
            deleted => deleted,
            moderate => moderate,
            blacklisted => blacklisted.map(|rule| context! { rule => rule, show_url => show_url }),
            reactions => reactions,
            edit => edit,
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
    let sizes = &state.media.config().thumbnail_sizes;
    let box_size = sizes.first().copied().unwrap_or(250);
    let kind = format!("thumb-{box_size}");
    let visible = visibility(&page.current);
    Ok(posts::cards(db, &ids, (&kind, &kind))
        .await?
        .iter()
        .filter(|card| {
            let status: Option<PostStatus> = card.status.parse().ok();
            let rating = card.rating.parse().unwrap_or(Rating::Explicit);
            status.is_some_and(|s| visible.statuses.contains(&s))
                && blacklist.is_none_or(|list| list.matching(rating, &card.tag_ids).is_none())
        })
        .map(|card| card_context(state, card, box_size, None))
        .collect())
}

/// Similar posts listed on a post page.
const SIMILAR_SHOWN: i64 = 12;

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
    let sizes = &state.media.config().thumbnail_sizes;
    let box_size = sizes.first().copied().unwrap_or(250);
    let kind = format!("thumb-{box_size}");
    let cards = posts::cards(db, &ids, (&kind, &kind)).await?;
    Ok(Some(context! {
        is_child => post.parent_id.is_some(),
        root => root,
        cards => cards
            .iter()
            .map(|card| context! {
                ..card_context(state, card, box_size, None),
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
    use sqlx::PgPool;
    use uwuu_core::permissions::SystemRole;

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
            .parse()
            .unwrap()
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
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

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
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

        assert_eq!(
            app.get(&format!("/posts/{}", id + 100), None).await.status,
            StatusCode::NOT_FOUND
        );
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
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

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
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
        assert_eq!(
            app.get("/posts?page=x", None).await.status,
            StatusCode::BAD_REQUEST
        );
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn blacklists_hide_posts(pool: PgPool) {
        uwuu_db::settings::set(
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
        let explicit = upload(&app, &alice, &fixture::png(20, 20), &[("rating", "e")]).await;
        let cat = upload(&app, &alice, &fixture::png(24, 20), &[("tags", "cat")]).await;
        let fine = upload(&app, &alice, &fixture::png(28, 20), &[]).await;

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

        let post = app.get(&format!("/posts/{explicit}"), None).await.body;
        assert!(post.contains("matches your blacklist"), "{post}");
        assert!(!post.contains("<img src=\"/data/"));
        let shown = app
            .get(&format!("/posts/{explicit}?blacklist=off"), None)
            .await
            .body;
        assert!(!shown.contains("matches your blacklist"));
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn post_pages_list_similar_posts(pool: PgPool) {
        let (app, _) = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let a = upload(&app, &alice, &fixture::png(20, 20), &[]).await;
        let b = upload(&app, &alice, &fixture::png(24, 20), &[]).await;
        let c = upload(&app, &alice, &fixture::png(28, 20), &[("rating", "e")]).await;
        // As if processing found them nearly identical.
        for (post, hash) in [(a, 0x1234_i64), (b, 0x1235), (c, 0x1237)] {
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
        uwuu_db::settings::set(&pool, "default_blacklist", serde_json::json!("rating:e"))
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
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
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

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn pending_posts_are_hidden_from_others(pool: PgPool) {
        uwuu_db::settings::set(&pool, "upload_approval", serde_json::json!(true))
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
