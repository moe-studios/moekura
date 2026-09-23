//! The post grid (front page) and single post pages.

use axum::Router;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use minijinja::{Value, context};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use uwuu_core::permissions::Permission;
use uwuu_core::posts::PostStatus;
use uwuu_core::search::{Order, Query as SearchQuery};
use uwuu_db::media::{self, Variant};
use uwuu_db::posts::{self, Card, Visibility};
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
}

/// Tags listed beside search results.
const SIDEBAR_TAGS: usize = 25;

/// Search results; the front page is the empty search.
async fn index(page: Page, Query(params): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.db.read();
    let config = &state.config.search;
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
    let post_query = (!normalized.is_empty()).then(|| {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("q", &normalized)
            .finish()
    });
    let card_values: Vec<Value> = cards
        .iter()
        .map(|card| card_context(state, card, box_size, post_query.as_deref()))
        .collect();

    let sidebar = sidebar_tags(db, &cards, &normalized).await?;
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
struct ShowQuery {
    /// The search the post was opened from.
    #[serde(default)]
    q: String,
}

async fn show(
    page: Page,
    Path(id): Path<i64>,
    Query(params): Query<ShowQuery>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
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
    let tag_groups = crate::tags::grouped(&categories, tags::by_ids(db, &post.tag_ids).await?);
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
    Ok(page.render(
        "post.html",
        context! {
            post => post_context,
            file => file,
            uploader => uploader,
            tag_groups => tag_groups,
            search => context! {
                tags => params.q,
                back_url => (!params.q.is_empty()).then(|| Value::from_safe_string(search_url(&params.q))),
            },
            processing => asset.processed_at.is_none(),
        },
    ))
}

fn is_web_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

fn human_size(bytes: i64) -> String {
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
        let first = page.body.find(&format!("href=\"/posts/{newer}\"")).unwrap();
        let second = page.body.find(&format!("href=\"/posts/{older}\"")).unwrap();
        assert!(first < second);
        // Not processed yet: placeholders, not broken images.
        assert!(
            page.body.contains("class=\"thumb pending-media\""),
            "{}",
            page.body
        );

        let page = app.get(&format!("/?page=b{newer}"), None).await;
        assert!(!page.body.contains(&format!("href=\"/posts/{newer}\"")));
        assert!(page.body.contains(&format!("href=\"/posts/{older}\"")));
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
