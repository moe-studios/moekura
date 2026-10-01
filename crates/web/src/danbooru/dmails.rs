//! `/dmails.json` (your messages), `/dmails/{id}.json`, and sending with
//! `POST /dmails.json`.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use moekura_db::dmails::{self, Dmail, Folder};
use serde::{Deserialize, Serialize};

use super::tags::window;
use super::{Fields, ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/dmails", get(index).post(create))
        .route("/dmails/{id}", get(show))
}

#[derive(Debug, Serialize)]
struct DanbooruDmail {
    id: i64,
    owner_id: i64,
    from_id: Option<i64>,
    to_id: Option<i64>,
    title: String,
    body: String,
    is_read: bool,
    is_deleted: bool,
    is_spam: bool,
    created_at: String,
    updated_at: String,
}

impl From<Dmail> for DanbooruDmail {
    fn from(d: Dmail) -> Self {
        let at = timestamp(d.created_at);
        Self {
            id: d.id,
            owner_id: d.owner_id,
            from_id: d.from_id,
            to_id: d.to_id,
            title: d.title,
            body: d.body,
            is_read: d.is_read,
            is_deleted: d.is_deleted,
            is_spam: false,
            updated_at: at.clone(),
            created_at: at,
        }
    }
}

fn owner(current: &CurrentUser) -> Result<i64, AppError> {
    current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)
}

#[derive(Debug, Default, Deserialize)]
struct Params {
    /// `received`, `sent` or `all` (the default).
    #[serde(rename = "search[folder]", default)]
    folder: String,
    #[serde(rename = "search[is_read]", default)]
    is_read: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<Params>,
) -> Result<Response, AppError> {
    let me = owner(&current)?;
    let (offset, limit) = window(&params.list, 1000)?;
    let folder = match params.folder.trim() {
        "received" | "inbox" => Folder::Inbox,
        "sent" => Folder::Sent,
        _ => Folder::All,
    };
    let unread = matches!(params.is_read.trim(), "false" | "no" | "0");
    let list: Vec<DanbooruDmail> =
        dmails::list(state.db.primary(), me, folder, unread, offset, limit)
            .await?
            .into_iter()
            .map(DanbooruDmail::from)
            .collect();
    json(list, &params.list.only)
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let me = owner(&current)?;
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let d = dmails::by_id(state.db.primary(), me, id)
        .await?
        .ok_or(AppError::NotFound)?;
    json(DanbooruDmail::from(d), "")
}

async fn create(
    State(state): State<AppState>,
    current: CurrentUser,
    fields: Fields,
) -> Result<Response, AppError> {
    let me = owner(&current)?;
    let field = |name: &str| fields.get(&format!("dmail[{name}]")).unwrap_or_default();
    let to = match field("to_name") {
        "" => {
            let id: i64 = field("to_id")
                .trim()
                .parse()
                .map_err(|_| AppError::Unprocessable("`dmail[to_name]` is required".into()))?;
            moekura_db::users::by_id(state.db.primary(), id)
                .await?
                .map(|u| u.name)
                .ok_or_else(|| AppError::Unprocessable("There's no such user.".into()))?
        }
        name => name.to_owned(),
    };
    let (copies, _) =
        crate::dmails::send(&state, &current, &to, field("title"), field("body")).await?;
    let sent = dmails::by_id(state.db.primary(), me, copies.sender_copy)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, axum::Json(DanbooruDmail::from(sent))).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::app;
    use crate::test_support::session_for;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn dmails(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        assert_eq!(
            app.get("/dmails.json", None).await.status,
            StatusCode::UNAUTHORIZED
        );
        let body = json!({ "dmail": { "to_name": "bob", "title": "Hi", "body": "Hello" } });
        let sent = app
            .json("POST", "/dmails.json", Some(&alice), Some(body))
            .await;
        assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.body);
        let parse = |body: &str| serde_json::from_str::<Value>(body).unwrap();
        let inbox = parse(
            &app.get("/dmails.json?search[folder]=received", Some(&bob))
                .await
                .body,
        );
        assert_eq!(inbox[0]["title"], "Hi");
        assert_eq!(inbox[0]["is_read"], false);
        let id = inbox[0]["id"].as_i64().unwrap();
        assert_eq!(
            parse(
                &app.get(&format!("/dmails/{id}.json"), Some(&bob))
                    .await
                    .body
            )["body"],
            "Hello"
        );
        assert_eq!(
            app.get(&format!("/dmails/{id}.json"), Some(&alice))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
}
