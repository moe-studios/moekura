//! Link previews: OpenGraph and Twitter card tags on post, pool and wiki
//! pages, so links unfurl in chat apps and social networks, and oEmbed for
//! posts. Private sites show none, and questionable and explicit posts
//! show no media unless the site allows it.

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

/// Whether a post of `rating` may show its image or video in a preview.
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

pub(crate) struct Video {
    pub url: String,
    pub content_type: &'static str,
    pub width: i32,
    pub height: i32,
}

/// Use the original video, even while its poster is still being generated.
pub(crate) fn post_video(state: &AppState, asset: &media::Asset, rating: Rating) -> Option<Video> {
    if !enabled(state) || !shows_image(state, rating) {
        return None;
    }
    let content_type = match asset.media_type.as_str() {
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => return None,
    };
    let key = moekura_storage::Key::parse(&asset.storage_key)?;
    Some(Video {
        url: absolute_url(state, &state.file_url(&key)),
        content_type,
        width: asset.width,
        height: asset.height,
    })
}

/// The tags a preview puts in the page head, or `None` on private sites.
pub(crate) fn meta(
    state: &AppState,
    path: &str,
    title: &str,
    description: &str,
    image: Option<Image>,
    video: Option<Video>,
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
        video => video.map(|v| context! {
            url => safe(&v.url),
            secure_url => v.url.starts_with("https://").then(|| safe(&v.url)),
            content_type => Value::from_safe_string(v.content_type.to_owned()),
            width => v.width,
            height => v.height,
        }),
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

/// A post's preview title: `Post #12`.
pub(crate) fn post_title(id: i64) -> String {
    format!("Post #{id}")
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
    let author = match post.uploader_id {
        Some(user) => users::by_id(db, user).await?.map(|u| u.name),
        None => None,
    };
    let site = state.site.get().settings.site_name.clone();
    let mut body = json!({
        "version": "1.0",
        "type": "link",
        "title": post_title(id),
        "provider_name": site,
        "provider_url": absolute_url(&state, "/"),
        "author_name": author,
    });
    let video = media::for_post(db, id)
        .await?
        .and_then(|asset| post_video(&state, &asset, post.rating));
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
        if video.is_some() {
            // Keep video posts as links with thumbnails. Advertising the
            // poster as a photo can override the page's OpenGraph video.
            body["thumbnail_url"] = json!(image.url);
            body["thumbnail_width"] = json!((f64::from(image.width) * scale).round() as i64);
            body["thumbnail_height"] = json!((f64::from(image.height) * scale).round() as i64);
        } else {
            body["type"] = json!("photo");
            body["url"] = json!(image.url);
            body["width"] = json!((f64::from(image.width) * scale).round() as i64);
            body["height"] = json!((f64::from(image.height) * scale).round() as i64);
        }
    }
    Ok(Json(body).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_core::posts::{PostStatus, Rating};
    use moekura_db::{media, posts};
    use moekura_storage::Key;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::test_support::{
        TestApp, fixture, session_for, test_config, test_state, test_state_with,
    };

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
            .split('?')
            .next()
            .unwrap()
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
                "<meta property=\"og:title\" content=\"Post #{safe}\">"
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
        assert!(head.contains(&format!(
            "<meta name=\"twitter:title\" content=\"Post #{safe}\">"
        )));
        assert!(head.contains("<meta property=\"og:type\" content=\"website\">"));
        assert!(!head.contains("og:video"));
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
        assert_eq!(body["title"], format!("Post #{safe}"));
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

    async fn video_post(pool: &PgPool, format: &str) -> (i64, i64, Key, Key) {
        let id = posts::insert(
            pool,
            posts::NewPost {
                uploader_id: None,
                rating: Rating::General,
                status: PostStatus::Active,
                source: "",
                description: "A video",
                tag_ids: &[],
            },
        )
        .await
        .unwrap();
        let hash = [id as u8; 32];
        let key = Key::original(&hex::encode(hash), format);
        let poster = Key::variant("poster", &hex::encode(hash), "jpg");
        let asset_id = media::insert(
            pool,
            media::NewAsset {
                post_id: id,
                sha256: &hash,
                md5: &[id as u8; 16],
                media_type: format,
                width: 640,
                height: 360,
                duration_ms: Some(1000),
                frames: 30,
                has_audio: true,
                file_size: 1000,
                storage_key: key.as_str(),
            },
        )
        .await
        .unwrap();
        (id, asset_id, key, poster)
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn videos_unfurl_with_and_without_posters(pool: PgPool) {
        let mut config = test_config();
        config.storage.public_base_url = Some("https://cdn.example/media/".parse().unwrap());
        let state = test_state_with(&pool, config).await;
        let app = TestApp::new(state, super::routes().merge(crate::posts::routes()));
        for format in ["mp4", "webm"] {
            let (id, asset_id, key, poster) = video_post(&pool, format).await;
            for with_poster in [false, true] {
                if with_poster {
                    media::save_variant(
                        &pool,
                        &media::Variant {
                            asset_id,
                            kind: "poster".into(),
                            format: "jpg".into(),
                            width: 320,
                            height: 180,
                            file_size: 100,
                            storage_key: poster.as_str().into(),
                        },
                    )
                    .await
                    .unwrap();
                }
                let page = app.get(&format!("/posts/{id}"), None).await;
                assert_eq!(page.status, StatusCode::OK, "{}", page.body);
                let head = page.body.split_once("</head>").unwrap().0;
                let video_url = format!("https://cdn.example/media/{}", key.as_str());
                for (property, content) in [
                    ("og:type", "video.other".to_owned()),
                    ("og:video", video_url.clone()),
                    ("og:video:url", video_url.clone()),
                    ("og:video:secure_url", video_url),
                    ("og:video:type", format!("video/{format}")),
                    ("og:video:width", "640".into()),
                    ("og:video:height", "360".into()),
                ] {
                    assert!(
                        head.contains(&format!(
                            "<meta property=\"{property}\" content=\"{content}\">"
                        )),
                        "{head}"
                    );
                }
                assert_eq!(head.contains("og:image"), with_poster, "{head}");
                let response = app
                    .get(
                        &format!(
                            "/oembed?url=http%3A%2F%2Flocalhost%3A8080%2Fposts%2F{id}&maxwidth=160"
                        ),
                        None,
                    )
                    .await;
                let body: Value = serde_json::from_str(&response.body).unwrap();
                assert_eq!(body["type"], "link");
                assert!(body.get("url").is_none(), "{body}");
                if with_poster {
                    assert_eq!(
                        body["thumbnail_url"],
                        format!("https://cdn.example/media/{}", poster.as_str())
                    );
                    assert_eq!(body["thumbnail_width"], 160);
                    assert_eq!(body["thumbnail_height"], 90);
                } else {
                    assert!(body.get("thumbnail_url").is_none(), "{body}");
                }
            }
        }
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn video_previews_respect_visibility(pool: PgPool) {
        let (id, _, _, _) = video_post(&pool, "mp4").await;
        let app = app(&pool).await;
        for (rating, shown) in [("g", true), ("s", true), ("q", false), ("e", false)] {
            sqlx::query("UPDATE posts SET rating = $1 WHERE id = $2")
                .bind(rating)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            let page = app.get(&format!("/posts/{id}"), None).await;
            assert_eq!(page.body.contains("og:video"), shown, "{}", page.body);
            assert!(
                !page.body.contains("og:video:secure_url"),
                "HTTP URLs aren't secure URLs"
            );
        }
        moekura_db::settings::set(&pool, "preview_all_ratings", serde_json::json!(true))
            .await
            .unwrap();
        let app = self::app(&pool).await;
        assert!(
            app.get(&format!("/posts/{id}"), None)
                .await
                .body
                .contains("og:video")
        );

        let admin = session_for(&pool, "admin", SystemRole::Admin).await;
        for (status, shown) in [("flagged", true), ("pending", false), ("deleted", false)] {
            sqlx::query("UPDATE posts SET status = $1 WHERE id = $2")
                .bind(status)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
            let page = app.get(&format!("/posts/{id}"), Some(&admin)).await;
            assert_eq!(page.status, StatusCode::OK, "{}", page.body);
            assert_eq!(page.body.contains("og:video"), shown, "{}", page.body);
        }
        sqlx::query("UPDATE posts SET status = 'active' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE roles SET permissions = permissions & ~1::bigint WHERE system_key = 'anonymous'")
            .execute(&pool).await.unwrap();
        let app = self::app(&pool).await;
        let page = app.get(&format!("/posts/{id}"), Some(&admin)).await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.body);
        assert!(!page.body.contains("og:video"));
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
