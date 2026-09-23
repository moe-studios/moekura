//! The post grid (front page) and single post pages.

use axum::Router;
use axum::extract::{Path, Query};
use axum::response::Response;
use axum::routing::get;
use minijinja::{Value, context};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use uwuu_core::permissions::Permission;
use uwuu_core::posts::PostStatus;
use uwuu_db::media::{self, Variant};
use uwuu_db::posts::{self, Card, Visibility};
use uwuu_db::{tags, users};
use uwuu_storage::Key;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::pages::Page;

/// Posts per page of the grid.
const PAGE_SIZE: i64 = 40;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
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

#[derive(Debug, Deserialize)]
struct IndexQuery {
    before: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let sizes = &state.media.config().thumbnail_sizes;
    let box_size = sizes.first().copied().unwrap_or(250);
    let kinds = (
        format!("thumb-{box_size}"),
        format!("thumb-{}", sizes.get(1).copied().unwrap_or(box_size)),
    );
    let cards = posts::recent(
        state.db.read(),
        &visibility(&page.current),
        query.before,
        PAGE_SIZE,
        (&kinds.0, &kinds.1),
    )
    .await?;
    let next_before = (cards.len() == PAGE_SIZE as usize)
        .then(|| cards.last().map(|c| c.id))
        .flatten();
    let cards: Vec<Value> = cards
        .iter()
        .map(|card| card_context(state, card, box_size))
        .collect();
    Ok(page.render(
        "home.html",
        context! { cards => cards, next_before => next_before },
    ))
}

/// The URL of a stored file, for templates. Built from a validated key and
/// the configured base URL, so it is marked safe (escaping would turn `/`
/// into `&#x2f;`).
fn file_url(state: &AppState, key: &str) -> Option<Value> {
    Key::parse(key).map(|k| Value::from_safe_string(state.storage.url(&k)))
}

fn card_context(state: &AppState, card: &Card, box_size: u32) -> Value {
    let url = |key: &Option<String>| key.as_deref().and_then(|k| file_url(state, k));
    let (width, height) = fit(card.width, card.height, box_size);
    context! {
        id => card.id,
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

async fn show(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
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
            .unwrap()
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

        let page = app.get(&format!("/?before={newer}"), None).await;
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
