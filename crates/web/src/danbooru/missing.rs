//! Answers for Danbooru features Moekura doesn't have, so clients carry on
//! instead of failing: their lists are empty (single items are already
//! 404, like any unknown URL). Also `/explore/posts/popular.json`, from
//! the best-scored posts of a day, week or month.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::search::PageRef;
use serde::Deserialize;
use time::{Date, Duration, OffsetDateTime};

use super::{ListParams, json};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

/// Lists of things Moekura doesn't have.
pub(crate) const EMPTY_LISTS: &[&str] = &[
    "/artists",
    "/artist_urls",
    "/artist_commentaries",
    "/forum_topics",
    "/forum_posts",
    "/forum_post_votes",
    "/dmails",
    "/user_feedbacks",
    "/user_name_change_requests",
    "/media_assets",
    "/iqdb_queries",
    "/explore/posts/viewed",
    "/explore/posts/searches",
    "/users/{id}/uploads",
];

pub(super) fn routes() -> Router<AppState> {
    let empty = Router::new().route("/explore/posts/popular", get(popular));
    EMPTY_LISTS
        .iter()
        .fold(empty, |router, path| router.route(path, get(nothing)))
}

async fn nothing() -> Result<Response, AppError> {
    json(Vec::<serde_json::Value>::new(), "")
}

#[derive(Debug, Default, Deserialize)]
struct PopularParams {
    #[serde(default)]
    date: String,
    #[serde(default)]
    scale: String,
    #[serde(flatten)]
    list: ListParams,
}

/// The search for the popular posts of `scale` (`day`, `week`, `month`)
/// around `date` (by default today).
fn popular_search(date: &str, scale: &str, today: Date) -> Result<String, AppError> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    let date = match date.get(..10) {
        Some(day) => Date::parse(day, format)
            .map_err(|_| AppError::BadRequest("`date` must look like 2026-01-31".into()))?,
        None if date.is_empty() => today,
        None => {
            return Err(AppError::BadRequest(
                "`date` must look like 2026-01-31".into(),
            ));
        }
    };
    let day = |d: Date| d.format(format).unwrap_or_default();
    let range = match scale {
        "" | "day" => day(date),
        // The seven days ending on the date.
        "week" => format!("{}..{}", day(date - Duration::days(6)), day(date)),
        "month" => format!("{}-{:02}", date.year(), u8::from(date.month())),
        _ => {
            return Err(AppError::BadRequest(
                "`scale` must be day, week or month".into(),
            ));
        }
    };
    Ok(format!("date:{range} order:score"))
}

async fn popular(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<PopularParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let search = popular_search(
        &params.date,
        &params.scale,
        OffsetDateTime::now_utc().date(),
    )?;
    let page: PageRef = match params.list.page.trim() {
        "" => PageRef::default(),
        page => page
            .parse()
            .map_err(|()| AppError::BadRequest("`page` must be a number".into()))?,
    };
    let limit = params.list.limit(state.config.search.max_per_page);
    let found = super::posts::find(&state, &current, &search, limit, page).await?;
    let db = state.reader(&current);
    json(
        super::posts::danbooru_posts(&state, db, found).await?,
        &params.list.only,
    )
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use time::macros::date;

    use super::popular_search;
    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    #[test]
    fn popular_ranges() {
        let today = date!(2026 - 03 - 10);
        let search = |d: &str, s: &str| popular_search(d, s, today).unwrap();
        assert_eq!(search("", ""), "date:2026-03-10 order:score");
        assert_eq!(
            search("2026-01-31T00:00:00Z", "week"),
            "date:2026-01-25..2026-01-31 order:score"
        );
        assert_eq!(search("2026-01-31", "month"), "date:2026-01 order:score");
        assert!(popular_search("soon", "day", today).is_err());
        assert!(popular_search("", "year", today).is_err());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn missing_features_are_empty(pool: PgPool) {
        let app = app(&pool).await;
        for path in [
            "/artists.json",
            "/forum_topics.json?search[id]=1",
            "/users/1/uploads.json",
        ] {
            let response = app.get(path, None).await;
            assert_eq!(response.status, StatusCode::OK, "{path}");
            assert_eq!(response.body, "[]", "{path}");
        }
        let missing = app.get("/artists/1.json", None).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        let body: Value = serde_json::from_str(&missing.body).unwrap();
        assert_eq!(body["success"], json!(false));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn popular_posts(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let low = upload(&app, &alice, 20, "cat").await;
        let high = upload(&app, &alice, 24, "cat").await;
        sqlx::query("UPDATE posts SET score = 5 WHERE id = $1")
            .bind(high)
            .execute(&pool)
            .await
            .unwrap();
        let response = app
            .get("/explore/posts/popular.json?scale=week", None)
            .await;
        let posts: Value = serde_json::from_str(&response.body).unwrap();
        let ids: Vec<i64> = posts
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, [high, low]);
        let old = app
            .get("/explore/posts/popular.json?date=2001-01-01", None)
            .await;
        assert_eq!(old.body, "[]");
    }
}
