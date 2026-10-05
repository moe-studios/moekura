//! `/iqdb_queries.json`: searching by image, as Danbooru's IQDB answers
//! it: `[{post_id, score, post}]`, score in percent. GET takes `url` or
//! `post_id` (or their `search[…]` forms); POST also takes a `file`.

use axum::Router;
use axum::extract::{FromRequest, Multipart, Query, Request, State};
use axum::http::header::CONTENT_TYPE;
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use serde::{Deserialize, Serialize};

use super::posts::{DanbooruPost, danbooru_posts};
use super::{Fields, json};
use crate::AppState;
use crate::auth::{CurrentUser, RequestInfo};
use crate::error::AppError;
use crate::image_search::Asked;

pub(super) fn routes(max_upload_bytes: u64) -> Router<AppState> {
    Router::new().route(
        "/iqdb_queries",
        get(query)
            .post(post)
            .layer(crate::upload::body_limit(max_upload_bytes)),
    )
}

#[derive(Debug, Serialize)]
struct IqdbMatch {
    post_id: i64,
    score: f64,
    post: DanbooruPost,
}

#[derive(Debug, Default, Deserialize)]
struct Params {
    #[serde(default)]
    url: String,
    #[serde(rename = "search[url]", default)]
    search_url: String,
    #[serde(default)]
    post_id: String,
    #[serde(rename = "search[post_id]", default)]
    search_post_id: String,
}

async fn answer(
    state: &AppState,
    current: &CurrentUser,
    info: &RequestInfo,
    asked: &Asked,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let matches = asked.run(state, current, info).await?.ok_or_else(|| {
        AppError::Unprocessable("Give `search[url]`, `search[post_id]` or a `file`.".into())
    })?;
    let db = state.reader(current);
    let ids: Vec<i64> = matches.iter().map(|m| m.post_id).collect();
    let found = moekura_db::posts::by_ids(db, &ids).await?;
    let mut posts = danbooru_posts(state, db, found).await?;
    let list: Vec<IqdbMatch> = matches
        .iter()
        .filter_map(|m| {
            let at = posts.iter().position(|p| p.id == m.post_id)?;
            Some(IqdbMatch {
                post_id: m.post_id,
                score: m.similarity(),
                post: posts.swap_remove(at),
            })
        })
        .collect();
    json(list, "")
}

async fn query(
    State(state): State<AppState>,
    current: CurrentUser,
    info: RequestInfo,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    let asked = Asked {
        file: None,
        url: [params.search_url, params.url]
            .into_iter()
            .find(|u| !u.trim().is_empty())
            .unwrap_or_default(),
        post_id: [params.search_post_id, params.post_id]
            .iter()
            .find_map(|p| p.trim().parse().ok()),
    };
    answer(&state, &current, &info, &asked).await
}

async fn post(
    State(state): State<AppState>,
    current: CurrentUser,
    info: RequestInfo,
    request: Request,
) -> Result<Response, AppError> {
    // Before the body, so a visitor to a private site can't make it
    // store files.
    current.require(Permission::ViewPosts)?;
    let multipart = request
        .headers()
        .get(CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"multipart/"));
    let asked = if multipart {
        let form = Multipart::from_request(request, &state)
            .await
            .map_err(|e| AppError::BadRequest(e.body_text()))?;
        Asked::from_multipart(&state, form).await?
    } else {
        let fields = Fields::from_request(request, &state).await?;
        let field = |names: [&str; 2]| names.iter().find_map(|n| fields.get(n)).unwrap_or_default();
        Asked {
            file: None,
            url: field(["search[url]", "url"]).to_owned(),
            post_id: field(["search[post_id]", "post_id"]).trim().parse().ok(),
        }
    };
    answer(&state, &current, &info, &asked).await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn searches_by_post(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let first = upload(&app, &alice, 20, "cat").await;
        let second = upload(&app, &alice, 24, "cat").await;
        // Hashes 1 bit apart.
        for (post, hash) in [(first, 0x0f0f_i64), (second, 0x0f0e_i64)] {
            let asset = moekura_db::media::for_post(&pool, post)
                .await
                .unwrap()
                .unwrap();
            moekura_db::media::mark_processed(&pool, asset.id, Some(hash as u64))
                .await
                .unwrap();
        }
        let found: Value = serde_json::from_str(
            &app.get(&format!("/iqdb_queries.json?search[post_id]={first}"), None)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(found[0]["post_id"].as_i64(), Some(second));
        assert_eq!(found[0]["score"].as_f64(), Some(98.4));
        assert_eq!(found[0]["post"]["id"].as_i64(), Some(second));
        assert_eq!(found.as_array().unwrap().len(), 1, "not the post itself");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn private_sites_refuse_before_reading_files(pool: PgPool) {
        sqlx::query("UPDATE roles SET permissions = permissions & ~1::bigint WHERE system_key = 'anonymous'")
            .execute(&pool)
            .await
            .unwrap();
        let app = app(&pool).await;
        let read = Arc::new(AtomicBool::new(false));
        let body = {
            let read = Arc::clone(&read);
            futures_util::stream::once(async move {
                read.store(true, Ordering::SeqCst);
                let part = "--b\r\nContent-Disposition: form-data; name=\"file\"; \
                            filename=\"a.png\"\r\n\r\npng\r\n--b--\r\n";
                Ok::<_, std::io::Error>(axum::body::Bytes::from(part))
            })
        };
        let request = Request::post("/iqdb_queries.json")
            .header("content-type", "multipart/form-data; boundary=b")
            .body(Body::from_stream(body))
            .unwrap();
        let response = app.raw(request).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(!read.load(Ordering::SeqCst), "the body was read");
    }
}
