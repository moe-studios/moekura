//! Wiki pages.

use axum::Json;
use axum::extract::{Path, Query, State};
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_db::wiki::{self, SaveError, Summary, Version, WikiPage};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::{AppError, ErrorBody};
use crate::tags::{MAX_PAGE, PAGE_SIZE};
use crate::wiki::{clean_body, conflict_message, title};

use super::tags::page_number;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiWikiPage {
    /// The tag name the page is about.
    pub title: String,
    /// The page's text, in the site's wiki markup.
    pub body: String,
    /// `body` as HTML, the way the site shows it.
    pub html: String,
    /// Counts up with every change; send it back when saving.
    pub version: i32,
    /// Who made the latest change.
    pub updater: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<WikiPage> for ApiWikiPage {
    fn from(p: WikiPage) -> Self {
        Self {
            html: markup::render(&p.body),
            title: p.title,
            body: p.body,
            version: p.version,
            updater: p.updater_name,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiWikiSummary {
    pub title: String,
    pub version: i32,
    pub updater: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

impl From<Summary> for ApiWikiSummary {
    fn from(s: Summary) -> Self {
        Self {
            title: s.title,
            version: s.version,
            updater: s.updater_name,
            updated_at: s.updated_at,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct WikiPageList {
    pub pages: Vec<ApiWikiSummary>,
    /// `page` for the next page; absent on the last one.
    pub next_page: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// A title prefix, or a pattern with `*` wildcards.
    #[serde(default)]
    title: String,
    /// 1-based.
    page: Option<i64>,
}

/// List wiki pages.
///
/// Most recently changed first, 50 per page. Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/wiki-pages",
    operation_id = "list_wiki_pages",
    tag = "wiki",
    params(ListParams),
    responses((status = 200, body = WikiPageList), (status = 400, body = ErrorBody)),
)]
pub(crate) async fn list(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ListParams>,
) -> Result<Json<WikiPageList>, AppError> {
    current.require(Permission::ViewPosts)?;
    let number = page_number(params.page)?;
    let pattern = moekura_core::tags::normalize(&params.title);
    let mut found = wiki::list(
        state.reader(&current),
        &pattern,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);
    Ok(Json(WikiPageList {
        pages: found.into_iter().map(ApiWikiSummary::from).collect(),
        next_page: has_next.then_some(number + 1),
    }))
}

/// Get a wiki page.
///
/// Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/wiki-pages/{title}",
    operation_id = "get_wiki_page",
    tag = "wiki",
    params(("title" = String, Path, description = "The tag name the page is about")),
    responses((status = 200, body = ApiWikiPage), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(raw): Path<String>,
) -> Result<Json<ApiWikiPage>, AppError> {
    current.require(Permission::ViewPosts)?;
    let title = title(&raw).map_err(|_| AppError::NotFound)?;
    let page = wiki::by_title(state.reader(&current), title.as_str())
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(page.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WikiChanges {
    /// The new text, in the site's wiki markup.
    body: String,
    /// The version the text was based on, 0 for a new page. If someone
    /// has changed the page since, the save is refused with 409. Leave it
    /// out to save over whatever is there.
    base_version: Option<i32>,
}

/// Create or change a wiki page.
///
/// Needs `edit_wiki`. Saving the text the page already has changes
/// nothing.
#[utoipa::path(
    put,
    path = "/wiki-pages/{title}",
    operation_id = "save_wiki_page",
    tag = "wiki",
    params(("title" = String, Path, description = "The tag name the page is about")),
    request_body = WikiChanges,
    responses(
        (status = 200, body = ApiWikiPage),
        (status = 409, body = ErrorBody, description = "The page changed since `base_version`"),
        (status = 422, body = ErrorBody, description = "The title isn't a valid tag name, or the text is empty or too long"),
    ),
)]
pub(crate) async fn save(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(raw): Path<String>,
    Json(changes): Json<WikiChanges>,
) -> Result<Json<ApiWikiPage>, AppError> {
    current.require(Permission::EditWiki)?;
    let title = title(&raw)?;
    let body = clean_body(&changes.body)?;
    crate::wiki::save(&state, &current, &title, &body, changes.base_version)
        .await
        .map_err(|e| match e {
            SaveError::Conflict { .. } => AppError::Conflict(conflict_message()),
            SaveError::Db(e) => e.into(),
        })?;
    let page = wiki::by_title(state.db.primary(), title.as_str())
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(page.into()))
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ApiWikiVersion {
    pub version: i32,
    pub updater: Option<String>,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<Version> for ApiWikiVersion {
    fn from(v: Version) -> Self {
        Self {
            version: v.version,
            updater: v.updater_name,
            body: v.body,
            created_at: v.created_at,
        }
    }
}

/// A wiki page's history.
///
/// Every version's text, newest first (at most 500). Needs `view_posts`.
#[utoipa::path(
    get,
    path = "/wiki-pages/{title}/versions",
    operation_id = "list_wiki_page_versions",
    tag = "wiki",
    params(("title" = String, Path, description = "The tag name the page is about")),
    responses((status = 200, body = Vec<ApiWikiVersion>), (status = 404, body = ErrorBody)),
)]
pub(crate) async fn versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(raw): Path<String>,
) -> Result<Json<Vec<ApiWikiVersion>>, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let title = title(&raw).map_err(|_| AppError::NotFound)?;
    let page = wiki::by_title(db, title.as_str())
        .await?
        .ok_or(AppError::NotFound)?;
    let versions = wiki::versions(db, page.id).await?;
    Ok(Json(
        versions.into_iter().map(ApiWikiVersion::from).collect(),
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::api::test_support::{app, json};
    use crate::test_support::session_for;

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn saves_and_reads_pages(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let body = json!({ "body": "A [b]cat[/b].", "base_version": 0 });

        let anonymous = app
            .json("PUT", "/api/v1/wiki-pages/cat", None, Some(body.clone()))
            .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        let saved = app
            .json(
                "PUT",
                "/api/v1/wiki-pages/Cat",
                Some(&alice),
                Some(body.clone()),
            )
            .await;
        assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);
        let page = json(&saved.body);
        assert_eq!(page["title"], json!("cat"));
        assert_eq!(page["version"], json!(1));
        assert_eq!(page["html"], json!("<p>A <strong>cat</strong>.</p>"));
        assert_eq!(page["updater"], json!("alice"));

        let stale = app
            .json("PUT", "/api/v1/wiki-pages/cat", Some(&alice), Some(body))
            .await;
        assert_eq!(stale.status, StatusCode::CONFLICT);
        let overwrite = app
            .json(
                "PUT",
                "/api/v1/wiki-pages/cat",
                Some(&alice),
                Some(json!({ "body": "A dog." })),
            )
            .await;
        assert_eq!(json(&overwrite.body)["version"], json!(2));
        let bad = app
            .json(
                "PUT",
                "/api/v1/wiki-pages/*",
                Some(&alice),
                Some(json!({ "body": "x" })),
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

        let shown = json(&app.get("/api/v1/wiki-pages/cat", None).await.body);
        assert_eq!(shown["body"], json!("A dog."));
        assert_eq!(
            app.get("/api/v1/wiki-pages/dog", None).await.status,
            StatusCode::NOT_FOUND
        );
        let list = json(&app.get("/api/v1/wiki-pages?title=c", None).await.body);
        assert_eq!(list["pages"][0]["title"], json!("cat"));
        assert_eq!(list["next_page"], json!(null));
        let versions = json(&app.get("/api/v1/wiki-pages/cat/versions", None).await.body);
        assert_eq!(versions[1]["body"], json!("A [b]cat[/b]."));
    }
}
