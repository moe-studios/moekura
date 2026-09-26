//! Link previews: OpenGraph and Twitter card tags on post, pool and wiki
//! pages, so links unfurl in chat apps and social networks, and oEmbed for
//! posts. Private sites show none, and questionable and explicit posts
//! show no image unless the site allows it.

use axum::Json;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use minijinja::{Value, context};
use moekura_core::posts::{PostStatus, Rating};
use moekura_db::{media, posts, users};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::api::absolute_url;
use crate::error::AppError;

/// Longest description, in characters.
const DESCRIPTION_LEN: usize = 200;

pub fn routes() -> Router<AppState> {
    Router::new().route("/oembed", get(oembed))
}

/// Whether link previews are shown at all: not on private sites.
pub(crate) fn enabled(state: &AppState) -> bool {
    !state.is_private()
}

/// Whether a post of `rating` may show its image in a preview.
pub(crate) fn shows_image(state: &AppState, rating: Rating) -> bool {
    matches!(rating, Rating::General | Rating::Sensitive)
        || state.site.get().settings.preview_all_ratings
}

fn shorten(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= DESCRIPTION_LEN {
        return text;
    }
    let cut: String = text.chars().take(DESCRIPTION_LEN).collect();
    let end = cut.rfind(' ').unwrap_or(cut.len());
    format!("{}…", &cut[..end])
}

/// An image for a preview: its URL (absolute) and size.
pub(crate) struct Image {
    pub url: String,
    pub width: i32,
    pub height: i32,
}

/// The tags a preview puts in the page head, or `None` on private sites.
pub(crate) fn meta(
    state: &AppState,
    path: &str,
    title: &str,
    description: &str,
    image: Option<Image>,
    oembed: bool,
) -> Option<Value> {
    if !enabled(state) {
        return None;
    }
    let url = absolute_url(state, path);
    let oembed_url = oembed.then(|| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("url", &url)
            .append_pair("format", "json")
            .finish();
        absolute_url(state, &format!("/oembed?{q}"))
    });
    // URLs as they are (with `&` escaped): escaping `/` too is valid HTML,
    // but some link scrapers don't undo it.
    let safe = |u: &str| crate::templates::url_value(u);
    Some(context! {
        site => state.site.get().settings.site_name,
        url => safe(&url),
        title => title,
        description => shorten(description),
        image => image.map(|i| context! { url => safe(&i.url), width => i.width, height => i.height }),
        oembed_url => oembed_url.as_deref().map(safe),
    })
}

/// The image a preview of post `id` shows, if any: its sample (or the
/// original for small stills and animations), or a video's poster.
pub(crate) async fn post_image(
    state: &AppState,
    db: &sqlx::PgPool,
    id: i64,
    rating: Rating,
) -> Result<Option<Image>, AppError> {
    if !shows_image(state, rating) {
        return Ok(None);
    }
    let Some(asset) = media::for_post(db, id).await? else {
        return Ok(None);
    };
    let variants = media::variants(db, asset.id).await?;
    let find = |kind: &str| variants.iter().find(|v| v.kind == kind);
    let video = matches!(asset.media_type.as_str(), "mp4" | "webm");
    let (key, width, height) = if video {
        match find("poster") {
            Some(poster) => (poster.storage_key.clone(), poster.width, poster.height),
            None => return Ok(None),
        }
    } else if let Some(sample) = find("sample").filter(|_| asset.frames <= 1) {
        (sample.storage_key.clone(), sample.width, sample.height)
    } else {
        (asset.storage_key.clone(), asset.width, asset.height)
    };
    let Some(key) = moekura_storage::Key::parse(&key) else {
        return Ok(None);
    };
    Ok(Some(Image {
        url: absolute_url(state, &state.file_url(&key)),
        width,
        height,
    }))
}

/// A post's preview title: `Post #12: tag tag tag`.
pub(crate) fn post_title(id: i64, tags: &[String]) -> String {
    let shown: Vec<&str> = tags.iter().take(8).map(String::as_str).collect();
    if shown.is_empty() {
        format!("Post #{id}")
    } else {
        format!("Post #{id}: {}", shown.join(" ").replace('_', " "))
    }
}

#[derive(Debug, Deserialize)]
struct OembedQuery {
    url: String,
    #[serde(default)]
    format: String,
    maxwidth: Option<i32>,
    maxheight: Option<i32>,
}

/// oEmbed for posts: `?url=https://site/posts/12`. JSON only.
async fn oembed(
    State(state): State<AppState>,
    Query(query): Query<OembedQuery>,
) -> Result<Response, AppError> {
    if !enabled(&state) {
        return Err(AppError::NotFound);
    }
    if !query.format.is_empty() && query.format != "json" {
        return Ok(axum::http::StatusCode::NOT_IMPLEMENTED.into_response());
    }
    let url = url::Url::parse(&query.url).map_err(|_| AppError::NotFound)?;
    let ours = url.origin() == state.config.server.public_url.origin();
    let id: i64 = url
        .path()
        .strip_prefix("/posts/")
        .and_then(|rest| rest.parse().ok())
        .filter(|_| ours)
        .ok_or(AppError::NotFound)?;
    let db = state.db.read();
    // What visitors may see: oEmbed consumers aren't logged in.
    let post = posts::by_id(db, id)
        .await?
        .filter(|p| matches!(p.status, PostStatus::Active | PostStatus::Flagged))
        .ok_or(AppError::NotFound)?;
    let mut tags: Vec<String> = moekura_db::tags::by_ids(db, &post.tag_ids)
        .await?
        .into_iter()
        .map(|t| t.name)
        .collect();
    tags.sort();
    let author = match post.uploader_id {
        Some(user) => users::by_id(db, user).await?.map(|u| u.name),
        None => None,
    };
    let site = state.site.get().settings.site_name.clone();
    let mut body = json!({
        "version": "1.0",
        "type": "link",
        "title": post_title(id, &tags),
        "provider_name": site,
        "provider_url": absolute_url(&state, "/"),
        "author_name": author,
    });
    if let Some(image) = post_image(&state, db, id, post.rating).await? {
        // Scaled down to fit maxwidth and maxheight, keeping its shape.
        let scale = [
            query
                .maxwidth
                .map(|w| f64::from(w) / f64::from(image.width.max(1))),
            query
                .maxheight
                .map(|h| f64::from(h) / f64::from(image.height.max(1))),
        ]
        .into_iter()
        .flatten()
        .fold(1.0_f64, f64::min);
        body["type"] = json!("photo");
        body["url"] = json!(image.url);
        body["width"] = json!((f64::from(image.width) * scale).round() as i64);
        body["height"] = json!((f64::from(image.height) * scale).round() as i64);
    }
    Ok(Json(body).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        let state = test_state(pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        TestApp::new(
            state,
            super::routes()
                .merge(crate::posts::routes())
                .merge(crate::upload::routes(max)),
        )
    }

    async fn upload(app: &TestApp, session: &str, rating: &str, width: u32) -> i64 {
        let fields = vec![
            ("rating", rating.to_owned()),
            ("tags", "cat_ears solo".to_owned()),
        ];
        let response = app
            .post_multipart(
                "/upload",
                Some(session),
                &fields,
                Some(("a.png", &fixture::png(width, 20))),
            )
            .await;
        response.location.unwrap()["/posts/".len()..]
            .parse()
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn posts_unfurl(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let safe = upload(&app, &alice, "g", 20).await;
        let explicit = upload(&app, &alice, "e", 24).await;

        let page = app.get(&format!("/posts/{safe}"), None).await;
        assert!(
            page.body.contains(&format!(
                "<meta property=\"og:title\" content=\"Post #{safe}: cat ears solo\">"
            )),
            "{}",
            page.body
        );
        let head = &page.body[..page.body.find("</head>").unwrap_or(0)];
        assert!(
            head.contains(
                "<meta property=\"og:image\" content=\"http://localhost:8080/data/original/"
            ),
            "{head}"
        );
        assert!(page.body.contains("application/json+oembed"));
        let page = app.get(&format!("/posts/{explicit}"), None).await;
        assert!(page.body.contains("og:title"));
        assert!(
            !page.body.contains("og:image"),
            "no image for explicit posts"
        );

        let oembed = |id: i64| {
            format!("/oembed?url=http%3A%2F%2Flocalhost%3A8080%2Fposts%2F{id}&maxwidth=10")
        };
        let body: Value = serde_json::from_str(&app.get(&oembed(safe), None).await.body).unwrap();
        assert_eq!(
            (&body["type"], &body["width"], &body["height"]),
            (
                &serde_json::json!("photo"),
                &serde_json::json!(10),
                &serde_json::json!(10)
            )
        );
        assert_eq!(body["author_name"], serde_json::json!("alice"));
        let body: Value =
            serde_json::from_str(&app.get(&oembed(explicit), None).await.body).unwrap();
        assert_eq!(body["type"], serde_json::json!("link"));
        assert_eq!(
            app.get(
                "/oembed?url=https%3A%2F%2Felsewhere.example%2Fposts%2F1",
                None
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );

        moekura_db::settings::set(&pool, "preview_all_ratings", serde_json::json!(true))
            .await
            .unwrap();
        let app = self::app(&pool).await;
        assert!(
            app.get(&format!("/posts/{explicit}"), None)
                .await
                .body
                .contains("og:image")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn private_sites_show_nothing(pool: PgPool) {
        sqlx::query("UPDATE roles SET permissions = permissions & ~1::bigint WHERE system_key = 'anonymous'")
            .execute(&pool)
            .await
            .unwrap();
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, "g", 20).await;
        assert!(
            !app.get(&format!("/posts/{id}"), Some(&alice))
                .await
                .body
                .contains("og:title")
        );
        assert_eq!(
            app.get(
                &format!("/oembed?url=http%3A%2F%2Flocalhost%3A8080%2Fposts%2F{id}"),
                None
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );
    }
}
