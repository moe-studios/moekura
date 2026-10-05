//! Searching by image (`/iqdb_queries`): a file, a link or a post is
//! hashed like every post's file, and the posts that look most like it
//! are listed with how alike they are, without making a post.

use axum::Router;
use axum::extract::{Multipart, Query};
use axum::response::Response;
use axum::routing::get;
use minijinja::context;
use moekura_core::permissions::Permission;
use moekura_db::{media, posts};
use serde::Deserialize;

use crate::AppState;
use crate::auth::{CurrentUser, RequestInfo};
use crate::error::AppError;
use crate::pages::Page;
use crate::upload::{LINK_FIELD_MAX, TempUpload, UploadError, text_field};

/// The most bits two hashes may differ by and still be listed (out of
/// 64): about 75% alike.
pub(crate) const MAX_DISTANCE: u32 = 16;
/// Matches listed.
pub(crate) const SHOWN: usize = 20;

pub fn routes(max_upload_bytes: u64) -> Router<AppState> {
    Router::new().route(
        "/iqdb_queries",
        get(form)
            .post(search_upload)
            .layer(crate::upload::body_limit(max_upload_bytes)),
    )
}

/// A match: the post and how many of the 64 hash bits differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Match {
    pub post_id: i64,
    pub distance: u32,
}

impl Match {
    /// How alike, in percent.
    pub fn similarity(self) -> f64 {
        (f64::from(64 - self.distance.min(64)) / 64.0 * 1000.0).round() / 10.0
    }
}

/// What to search with.
pub(crate) enum Needle<'a> {
    File(&'a TempUpload),
    Post(i64),
}

fn upload_error(error: UploadError) -> AppError {
    match error {
        UploadError::Invalid(message) | UploadError::Limit(message) => {
            AppError::Unprocessable(message)
        }
        UploadError::Duplicate(id) => AppError::Duplicate(id),
        UploadError::Similar(found) => AppError::Similar {
            posts: found.posts,
            staged: found.staged,
        },
        UploadError::TooFast(retry_after_secs) => AppError::TooManyRequests { retry_after_secs },
        UploadError::Internal(detail) => AppError::Internal(detail),
    }
}

/// Why a file couldn't be hashed: its own fault, or ours.
fn refused(e: moekura_media::MediaError) -> AppError {
    if e.is_internal() {
        AppError::Internal(e.to_string())
    } else {
        AppError::Unprocessable(format!("That file can't be searched with: {e}."))
    }
}

/// The perceptual hash of a file, as processing would make it.
pub(crate) async fn hash_file(state: &AppState, file: &TempUpload) -> Result<u64, AppError> {
    let kind = state.media.identify(file.path()).await.map_err(refused)?;
    hash_as(state, file, kind).await
}

/// The perceptual hash of a file to search with. Only pictures and
/// videos: a zip would be opened and unpacked to find its first frame,
/// too much to do for a search anyone may make. Uploads still compare
/// them, through [`hash_file`].
async fn hash_needle(state: &AppState, file: &TempUpload) -> Result<u64, AppError> {
    let kind = state.media.identify(file.path()).await.map_err(refused)?;
    if kind == moekura_media::MediaType::Ugoira {
        return Err(AppError::Unprocessable(
            "Search with a picture or a video, not a zip.".into(),
        ));
    }
    hash_as(state, file, kind).await
}

/// The perceptual hash of `file`, a `kind` file.
async fn hash_as(
    state: &AppState,
    file: &TempUpload,
    kind: moekura_media::MediaType,
) -> Result<u64, AppError> {
    let media = &state.media;
    let dir = state.work_dir.join(format!(
        "search-{}",
        hex::encode(&moekura_core::tokens::NewToken::generate().hash[..8])
    ));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let hashed = async {
        let probe = media.probe(file.path(), kind).await?;
        let (source, source_type) = media
            .still(file.path(), kind, probe.duration_ms, &dir)
            .await?;
        media.perceptual_hash(&source, source_type, &dir).await
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    hashed.map_err(refused)
}

/// The posts `current` may see that look most like `needle`, closest
/// first. [`Asked::run`] counts it against the searcher's allowance.
async fn search(
    state: &AppState,
    current: &CurrentUser,
    needle: Needle<'_>,
) -> Result<Vec<Match>, AppError> {
    let db = state.reader(current);
    let (hash, exclude) = match needle {
        Needle::File(file) => (hash_needle(state, file).await?, None),
        Needle::Post(id) => {
            let post = posts::by_id(db, id)
                .await?
                .filter(|p| crate::posts::visibility(current).allows(p))
                .ok_or(AppError::NotFound)?;
            let hash = media::for_post(db, post.id)
                .await?
                .and_then(|a| a.phash)
                .ok_or_else(|| {
                    AppError::Unprocessable("That post's file hasn't been processed yet.".into())
                })?;
            (hash as u64, Some(post.id))
        }
    };
    let found = media::nearest(db, hash, MAX_DISTANCE, SHOWN as i64 * 3).await?;
    let ids: Vec<i64> = found.iter().map(|s| s.post_id).collect();
    let visible = crate::posts::visibility(current);
    let shown = posts::by_ids(db, &ids).await?;
    Ok(found
        .iter()
        .filter(|s| Some(s.post_id) != exclude)
        .filter(|s| shown.iter().any(|p| p.id == s.post_id && visible.allows(p)))
        .take(SHOWN)
        .map(|s| Match {
            post_id: s.post_id,
            distance: u32::try_from(s.distance).unwrap_or(64),
        })
        .collect())
}

/// The largest file a link to search with may be (or the upload limit,
/// if that's lower). Visitors can search too, so a link may cost far
/// less than an upload's, and a search only needs one picture.
const MAX_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;

/// Downloads `url` to search with (a work's page gives its best file),
/// up to [`MAX_DOWNLOAD_BYTES`].
async fn fetch(state: &AppState, url: &str) -> Result<TempUpload, AppError> {
    let url = url.trim();
    let found = state.sources.lookup(url).await;
    crate::upload::check_found(url, found.as_deref()).map_err(upload_error)?;
    let file_url = match found.as_deref() {
        Some(info) if !info.files.is_empty() => info.files[0].as_str(),
        _ => url,
    };
    let limit = MAX_DOWNLOAD_BYTES.min(state.media.config().max_upload_mb * 1024 * 1024);
    crate::upload::download_within(state, file_url, found.as_deref(), limit)
        .await
        .map_err(upload_error)
}

/// A search's parameters from a form or an API: a file, a link or a post.
#[derive(Default)]
pub(crate) struct Asked {
    pub file: Option<TempUpload>,
    pub url: String,
    pub post_id: Option<i64>,
}

impl Asked {
    /// Reads `file` (or `search[file]`), `url` and `post_id` (or their
    /// `search[…]` forms) from a multipart body.
    pub async fn from_multipart(state: &AppState, mut form: Multipart) -> Result<Self, AppError> {
        let mut asked = Self::default();
        while let Some(field) = form
            .next_field()
            .await
            .map_err(|e| AppError::BadRequest(e.body_text()))?
        {
            match field.name().unwrap_or_default() {
                "file" | "search[file]" => {
                    if field.file_name().is_none_or(str::is_empty) {
                        continue;
                    }
                    asked.file = Some(
                        crate::upload::save_to_temp(state, field)
                            .await
                            .map_err(upload_error)?,
                    );
                }
                "url" | "search[url]" => {
                    asked.url = text_field(field, LINK_FIELD_MAX).await?;
                }
                "post_id" | "search[post_id]" => {
                    asked.post_id = text_field(field, 64).await?.trim().parse().ok();
                }
                _ => {}
            }
        }
        Ok(asked)
    }

    /// Runs the search, if anything was asked. Each counts against the
    /// searcher's allowance, as it compares with every post, before a
    /// link is looked up or downloaded.
    pub async fn run(
        &self,
        state: &AppState,
        current: &CurrentUser,
        info: &RequestInfo,
    ) -> Result<Option<Vec<Match>>, AppError> {
        let url = self.url.trim();
        if self.file.is_none() && url.is_empty() && self.post_id.is_none() {
            return Ok(None);
        }
        current.require(Permission::ViewPosts)?;
        let client = crate::rate_limit::client_key(current.user.as_ref().map(|u| u.id), info.ip);
        state.rate_limits.check_image_search(&client).await?;
        let fetched;
        let needle = if let Some(file) = &self.file {
            Needle::File(file)
        } else if let Some(id) = self.post_id.filter(|_| url.is_empty()) {
            Needle::Post(id)
        } else {
            fetched = fetch(state, url).await?;
            Needle::File(&fetched)
        };
        search(state, current, needle).await.map(Some)
    }
}

#[derive(Debug, Default, Deserialize)]
struct FormQuery {
    #[serde(default)]
    url: String,
    post_id: Option<i64>,
}

async fn render(page: &Page, asked: &Asked, info: &RequestInfo) -> Result<Response, AppError> {
    let found = match asked.run(page.state(), &page.current, info).await {
        Ok(found) => found,
        Err(AppError::Unprocessable(message)) => {
            return Ok(page.render_with_status(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "image_search.html",
                context! { url => asked.url, error => message },
            ));
        }
        Err(error) => return Err(error),
    };
    let results = match &found {
        Some(matches) => {
            let ids: Vec<i64> = matches.iter().map(|m| m.post_id).collect();
            let db = page.state().reader(&page.current);
            let cards = crate::posts::grid(page, db, &ids, None).await?;
            Some(
                cards
                    .into_iter()
                    .filter_map(|(id, card)| {
                        let m = matches.iter().find(|m| m.post_id == id)?;
                        Some(context! { card => card, similarity => m.similarity() })
                    })
                    .collect::<Vec<_>>(),
            )
        }
        None => None,
    };
    Ok(page.render(
        "image_search.html",
        context! {
            url => asked.url,
            post_id => asked.post_id,
            results => results,
        },
    ))
}

async fn form(
    page: Page,
    info: RequestInfo,
    Query(query): Query<FormQuery>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let asked = Asked {
        file: None,
        url: query.url,
        post_id: query.post_id,
    };
    render(&page, &asked, &info).await
}

async fn search_upload(
    page: Page,
    info: RequestInfo,
    form: Multipart,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let asked = Asked::from_multipart(page.state(), form).await?;
    if asked.file.is_none() && asked.url.trim().is_empty() && asked.post_id.is_none() {
        return Ok(page.render_with_status(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "image_search.html",
            context! { error => "Choose a picture, or paste a link to one." },
        ));
    }
    render(&page, &asked, &info).await
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[test]
    fn similarity() {
        assert_eq!(
            Match {
                post_id: 1,
                distance: 0
            }
            .similarity(),
            100.0
        );
        assert_eq!(
            Match {
                post_id: 1,
                distance: 16
            }
            .similarity(),
            75.0
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn finds_look_alikes(pool: PgPool) {
        let state = test_state(&pool).await;
        let app = TestApp::new(
            state.clone(),
            routes(10 * 1024 * 1024).merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let post = crate::danbooru::test_support::upload(&app, &alice, 64, "cat").await;
        // As processing would: the file's hash.
        let png = fixture::png(64, 20);
        let dir = std::env::temp_dir().join(format!("moekura-iqdb-{post}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        std::fs::write(&path, &png).unwrap();
        let hash = state
            .media
            .perceptual_hash(&path, moekura_media::MediaType::Png, &dir)
            .await
            .unwrap();
        let asset = media::for_post(&pool, post).await.unwrap().unwrap();
        media::mark_processed(&pool, asset.id, Some(hash))
            .await
            .unwrap();

        // The same picture, recompressed.
        let found = app
            .post_multipart("/iqdb_queries", None, &[], Some(("b.png", &png)))
            .await;
        assert_eq!(found.status, StatusCode::OK, "{}", found.body);
        assert!(
            found.body.contains(&format!("href=\"/posts/{post}")),
            "{}",
            found.body
        );
        assert!(found.body.contains("100.0% alike"), "{}", found.body);

        let none = app.post_multipart("/iqdb_queries", None, &[], None).await;
        assert_eq!(none.status, StatusCode::UNPROCESSABLE_ENTITY);
        let page = app.get("/iqdb_queries", None).await;
        assert_eq!(page.status, StatusCode::OK);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn searches_with_pictures_not_zips(pool: PgPool) {
        use std::io::Write;

        let state = test_state(&pool).await;
        let app = TestApp::new(state, routes(10 * 1024 * 1024));
        // An ugoira, as uploads take them.
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        zip.start_file("000000.png", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&fixture::png(16, 16)).unwrap();
        let zip = zip.finish().unwrap().into_inner();
        let refused = app
            .post_multipart("/iqdb_queries", None, &[], Some(("a.zip", &zip)))
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("not a zip"), "{}", refused.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn uploaded_zips_are_still_compared(pool: PgPool) {
        use std::io::Write;

        let state = test_state(&pool).await;
        let app = TestApp::new(state.clone(), crate::upload::routes(10 * 1024 * 1024));
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let fields = vec![("rating", "g".to_owned()), ("tags", "cat".to_owned())];
        let first = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &fields,
                Some(("a.png", &fixture::png(64, 64))),
            )
            .await;
        assert_eq!(first.status, StatusCode::SEE_OTHER, "{}", first.body);
        let original: i64 = first.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let frame = fixture::png(66, 66);
        crate::test_support::hash_like(&state, &pool, original, &frame).await;

        // The same picture as an ugoira: searches don't take it, but an
        // upload still warns that it looks like the post.
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for n in 0..2 {
            zip.start_file(format!("{n:06}.png"), options).unwrap();
            zip.write_all(&frame).unwrap();
        }
        zip.start_file("animation.json", options).unwrap();
        zip.write_all(
            br#"{"frames":[{"file":"000000.png","delay":50},{"file":"000001.png","delay":70}]}"#,
        )
        .unwrap();
        let zip = zip.finish().unwrap().into_inner();
        let warned = app
            .post_multipart("/upload", Some(&alice), &fields, Some(("b.zip", &zip)))
            .await;
        assert_eq!(warned.status, StatusCode::SEE_OTHER, "{}", warned.body);
        let staged: (i64, Option<i64>) =
            sqlx::query_as("SELECT upload_id, phash FROM staged_uploads")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            warned.location.as_deref(),
            Some(format!("/uploads/{}", staged.0).as_str())
        );
        assert!(staged.1.is_some());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn damaged_files_are_refused_without_server_details(pool: PgPool) {
        let state = test_state(&pool).await;
        let work_dir = state.work_dir.to_string_lossy().into_owned();
        let app = TestApp::new(state, routes(10 * 1024 * 1024));
        let broken = b"\x89PNG\r\n\x1a\nthis is not really a png";
        let refused = app
            .post_multipart("/iqdb_queries", None, &[], Some(("b.png", broken)))
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("damaged"), "{}", refused.body);
        assert!(!refused.body.contains(&work_dir), "{}", refused.body);
        assert!(!refused.body.contains("vips"), "{}", refused.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn api_by_post(pool: PgPool) {
        let state = test_state(&pool).await;
        let app = TestApp::new(
            state,
            crate::api::routes(10 * 1024 * 1024).merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let first = crate::danbooru::test_support::upload(&app, &alice, 20, "cat").await;
        let second = crate::danbooru::test_support::upload(&app, &alice, 24, "cat").await;
        for (post, hash) in [(first, 0xff00_u64), (second, 0xff01_u64)] {
            let asset = media::for_post(&pool, post).await.unwrap().unwrap();
            media::mark_processed(&pool, asset.id, Some(hash))
                .await
                .unwrap();
        }
        let found = app
            .post_multipart(
                "/api/v1/posts/similar",
                None,
                &[("post_id", first.to_string())],
                None,
            )
            .await;
        assert_eq!(found.status, StatusCode::OK, "{}", found.body);
        let found: serde_json::Value = serde_json::from_str(&found.body).unwrap();
        assert_eq!(found[0]["post"]["id"].as_i64(), Some(second));
        assert_eq!(found[0]["distance"], 1);
        let nothing = app
            .post_multipart("/api/v1/posts/similar", None, &[], None)
            .await;
        assert_eq!(nothing.status, StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// A local server with `origin`'s routes standing in for the web, and
    /// an app whose lookups and downloads may reach it.
    async fn app_with_origin(pool: &PgPool, origin: Router) -> (TestApp, AppState, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, origin).await });
        let mut state = test_state(pool).await;
        state.fetcher = crate::fetch::Fetcher::new(std::time::Duration::from_secs(10), true);
        state.sources = std::sync::Arc::new(crate::sources::Sources::new(
            true,
            moekura_core::config::SourcesConfig::default(),
        ));
        let routes = routes(10 * 1024 * 1024)
            .merge(crate::api::routes(10 * 1024 * 1024))
            .merge(crate::danbooru::test_support::routes());
        let app = TestApp::new(state.clone(), routes);
        (app, state, format!("http://{addr}"))
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn counts_searches_before_fetching_links(pool: PgPool) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        let png = fixture::png(20, 20);
        let origin = Router::new().route(
            "/a.png",
            get(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                let png = png.clone();
                async move { png }
            }),
        );
        let (app, state, origin) = app_with_origin(&pool, origin).await;
        // A visitor who has used up their searches.
        let visitor = crate::rate_limit::client_key(None, None);
        let mut allowed = 0;
        while state.rate_limits.check_image_search(&visitor).await.is_ok() {
            allowed += 1;
            assert!(allowed < 1000, "searches are limited");
        }
        let link = format!("{origin}/a.png");

        let page = app.get(&format!("/iqdb_queries?url={link}"), None).await;
        assert_eq!(page.status, StatusCode::TOO_MANY_REQUESTS, "{}", page.body);
        let danbooru = app
            .get(&format!("/iqdb_queries.json?url={link}"), None)
            .await;
        assert_eq!(danbooru.status, StatusCode::TOO_MANY_REQUESTS);
        let api = app
            .post_multipart("/api/v1/posts/similar", None, &[("url", link)], None)
            .await;
        assert_eq!(api.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing was fetched");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn downloads_far_less_than_uploads(pool: PgPool) {
        let origin = Router::new().route(
            "/big.png",
            get(|| async { vec![0u8; MAX_DOWNLOAD_BYTES as usize + 1] }),
        );
        let (app, state, origin) = app_with_origin(&pool, origin).await;
        assert!(state.media.config().max_upload_mb * 1024 * 1024 > MAX_DOWNLOAD_BYTES);
        let found = app
            .get(&format!("/iqdb_queries?url={origin}/big.png"), None)
            .await;
        assert_eq!(found.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(found.body.contains("larger than 20 MB"), "{}", found.body);
    }
}
