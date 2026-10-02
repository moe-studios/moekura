//! `/explore/posts/popular.json` and `viewed.json` (the best scored and
//! most viewed posts of a day, week or month), and `searches.json` and
//! `missed_searches.json` (`[query, count]` pairs), from the same counts
//! as the site's explore pages.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::posts;
use moekura_db::search::PageRef;
use serde::Deserialize;
use time::OffsetDateTime;

use super::{ListParams, json};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::explore::{self, Range};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/explore/posts/popular", get(popular))
        .route("/explore/posts/viewed", get(viewed))
        .route("/explore/posts/searches", get(searches))
        .route("/explore/posts/missed_searches", get(missed_searches))
}

#[derive(Debug, Default, Deserialize)]
struct RangeParams {
    #[serde(default)]
    date: String,
    #[serde(default)]
    scale: String,
    #[serde(flatten)]
    list: ListParams,
}

impl RangeParams {
    fn range(&self) -> Result<Range, AppError> {
        Range::parse(&self.date, &self.scale, OffsetDateTime::now_utc().date())
    }
}

async fn popular(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<RangeParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let page: PageRef = match params.list.page.trim() {
        "" => PageRef::default(),
        page => page
            .parse()
            .map_err(|()| AppError::BadRequest("`page` must be a number".into()))?,
    };
    let limit = params.list.limit(state.search_config().max_per_page);
    let ids = explore::popular_ids(&state, &current, &params.range()?, limit, page, &[]).await?;
    posts_json(&state, &current, &ids, &params.list.only).await
}

async fn viewed(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<RangeParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let limit = params.list.limit(state.search_config().max_per_page);
    let ids = explore::viewed_ids(&state, &current, &params.range()?, limit).await?;
    posts_json(&state, &current, &ids, &params.list.only).await
}

async fn posts_json(
    state: &AppState,
    current: &CurrentUser,
    ids: &[i64],
    only: &str,
) -> Result<Response, AppError> {
    let db = state.reader(current);
    let mut found = posts::by_ids(db, ids).await?;
    found.sort_by_key(|p| ids.iter().position(|id| *id == p.id));
    json(super::posts::danbooru_posts(state, db, found).await?, only)
}

async fn search_counts(
    state: AppState,
    current: CurrentUser,
    params: RangeParams,
    missed: bool,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let limit = params.list.limit(1000);
    let found =
        explore::top_searches(&state, &current, &params.range()?, missed, i64::from(limit)).await?;
    json(found, "")
}

async fn searches(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<RangeParams>,
) -> Result<Response, AppError> {
    search_counts(state, current, params, false).await
}

async fn missed_searches(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<RangeParams>,
) -> Result<Response, AppError> {
    search_counts(state, current, params, true).await
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use moekura_db::explore::{self, Searched};
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use time::OffsetDateTime;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    fn ids(body: &str) -> Vec<i64> {
        let posts: Value = serde_json::from_str(body).unwrap();
        posts
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_i64().unwrap())
            .collect()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn explore_lists(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let low = upload(&app, &alice, 20, "cat").await;
        let high = upload(&app, &alice, 24, "cat").await;
        sqlx::query("UPDATE posts SET score = 5 WHERE id = $1")
            .bind(high)
            .execute(&pool)
            .await
            .unwrap();
        let popular = app
            .get("/explore/posts/popular.json?scale=week", None)
            .await;
        assert_eq!(ids(&popular.body), [high, low]);
        let old = app
            .get("/explore/posts/popular.json?date=2001-01-01", None)
            .await;
        assert_eq!(old.body, "[]");

        let today = OffsetDateTime::now_utc().date();
        explore::add_views(&pool, &[(today, low, 5), (today, high, 1)])
            .await
            .unwrap();
        let viewed = app.get("/explore/posts/viewed.json", None).await;
        assert_eq!(ids(&viewed.body), [low, high]);

        let searched = |query: &str, searches, misses| Searched {
            day: today,
            query: query.into(),
            searches,
            misses,
        };
        explore::add_searches(&pool, &[searched("cat", 3, 0), searched("dgo", 1, 1)])
            .await
            .unwrap();
        let searches: Value =
            serde_json::from_str(&app.get("/explore/posts/searches.json", None).await.body)
                .unwrap();
        assert_eq!(searches, json!([["cat", 3], ["dgo", 1]]));
        let missed: Value = serde_json::from_str(
            &app.get("/explore/posts/missed_searches.json", None)
                .await
                .body,
        )
        .unwrap();
        assert_eq!(missed, json!([["dgo", 1]]));
    }
}
