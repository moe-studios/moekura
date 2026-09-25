//! `/profile.json`, `/users.json`, `/users/{id}.json`, `/wiki_pages.json`
//! and `/wiki_pages/{title or id}.json`.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::{Permission, Role, SystemRole};
use moekura_core::tags::normalize;
use moekura_core::user_settings::UserSettings;
use moekura_db::site_cache::SiteSnapshot;
use moekura_db::users::{self, User, UserStatus};
use moekura_db::wiki::{self, WikiPage};
use serde::{Deserialize, Serialize};

use super::{ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/profile", get(profile))
        .route("/users", get(index))
        .route("/users/{id}", get(show))
        .route("/wiki_pages", get(wiki_index))
        .route("/wiki_pages/{title}", get(wiki_show))
}

/// Danbooru's level for a role: the built-in role's, or for a role the
/// site added, that of the highest built-in role ranked at or below it.
fn level(role: Option<&Role>, site: &SiteSnapshot) -> i32 {
    let danbooru = |system: SystemRole| match system {
        SystemRole::Anonymous => 0,
        SystemRole::Member => 20,
        SystemRole::Contributor => 35,
        // Danbooru's Approver: reviews uploads.
        SystemRole::Janitor => 37,
        SystemRole::Moderator => 40,
        SystemRole::Admin => 50,
    };
    let Some(role) = role else { return 20 };
    if let Some(system) = role.system {
        return danbooru(system);
    }
    SystemRole::ALL
        .into_iter()
        .filter_map(|s| site.system_role(s).map(|r| (r.rank, s)))
        .filter(|(rank, _)| *rank <= role.rank)
        .max_by_key(|(rank, _)| *rank)
        .map_or(20, |(_, s)| danbooru(s))
}

/// A user as Danbooru describes them to anyone.
#[derive(Debug, Serialize)]
pub(super) struct DanbooruUser {
    id: i64,
    name: String,
    level: i32,
    level_string: String,
    inviter_id: Option<i64>,
    created_at: String,
    updated_at: String,
    last_logged_in_at: Option<String>,
    post_upload_count: i64,
    post_update_count: i64,
    note_update_count: i64,
    favorite_count: i64,
    comment_count: i64,
    forum_post_count: i64,
    favorite_group_count: i64,
    positive_feedback_count: i64,
    neutral_feedback_count: i64,
    negative_feedback_count: i64,
    is_banned: bool,
    is_deleted: bool,
}

pub(super) async fn danbooru_user(
    state: &AppState,
    db: &sqlx::PgPool,
    user: &User,
) -> Result<DanbooruUser, AppError> {
    let site = state.site.get();
    let role = site.role(user.role_id);
    let activity = users::activity(db, user.id).await?;
    let created = timestamp(user.created_at);
    Ok(DanbooruUser {
        id: user.id,
        name: user.name.clone(),
        level: level(role, &site),
        level_string: role.map_or_else(|| "Member".to_owned(), |r| r.name.clone()),
        inviter_id: None,
        updated_at: created.clone(),
        created_at: created,
        last_logged_in_at: user.last_seen_at.map(timestamp),
        post_upload_count: activity.uploads,
        post_update_count: activity.edits,
        note_update_count: 0,
        favorite_count: activity.favorites,
        comment_count: 0,
        forum_post_count: 0,
        favorite_group_count: 0,
        positive_feedback_count: 0,
        neutral_feedback_count: 0,
        negative_feedback_count: 0,
        is_banned: activity.banned,
        is_deleted: user.status == UserStatus::Deactivated,
    })
}

/// Who `current` may look up: active users, or anyone for staff who
/// manage users.
fn visible(current: &CurrentUser, user: &User) -> bool {
    user.status == UserStatus::Active || current.can(Permission::ManageUsers)
}

/// The account a key belongs to, with its preferences; gallery-dl reads
/// `blacklisted_tags`.
async fn profile(
    State(state): State<AppState>,
    current: CurrentUser,
) -> Result<Response, AppError> {
    let user = current.user.clone().ok_or(AppError::Unauthorized)?;
    let db = state.db.primary();
    let settings = UserSettings::from_json(&user.settings);
    let site = state.site.get();
    let public = danbooru_user(&state, db, &user).await?;
    let mut value = serde_json::to_value(public).map_err(|e| AppError::Internal(e.to_string()))?;
    let extra = serde_json::json!({
        "blacklisted_tags": settings.blacklist.unwrap_or_else(|| site.settings.default_blacklist.clone()),
        "favorite_tags": "",
        "per_page": settings.per_page.unwrap_or(state.config.search.per_page),
        "time_zone": "UTC",
        "default_image_size": "large",
        "comment_threshold": 0,
        "theme": "auto",
        "custom_style": "",
        "last_forum_read_at": null,
        "can_approve_posts": current.can(Permission::ApprovePosts),
        "can_upload_free": current.can(Permission::UploadWithoutApproval),
        "is_banned": current.ban.is_some(),
    });
    if let (Some(map), serde_json::Value::Object(extra)) = (value.as_object_mut(), extra) {
        map.extend(extra);
    }
    json(value, "")
}

#[derive(Debug, Default, Deserialize)]
struct UserParams {
    #[serde(rename = "search[id]", default)]
    id: String,
    #[serde(rename = "search[name]", default)]
    name: String,
    #[serde(rename = "search[name_matches]", default)]
    name_matches: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<UserParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    // Each user costs a few counts, so pages are small.
    let limit = i64::from(params.list.limit(100));
    let page: i64 = params.list.page.trim().parse().unwrap_or(1).clamp(1, 1000);
    let found: Vec<User> = if !params.id.trim().is_empty() {
        let ids: Vec<i64> = params
            .id
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        let mut found = users::by_ids(db, &ids).await?;
        found.sort_by_key(|u| u.id);
        found
    } else if !params.name.trim().is_empty() {
        users::by_name(db, params.name.trim())
            .await?
            .into_iter()
            .collect()
    } else {
        let prefix = params.name_matches.trim().trim_end_matches('*');
        let status = (!current.can(Permission::ManageUsers)).then_some(UserStatus::Active);
        users::list(db, prefix, status, (page - 1) * limit, limit).await?
    };
    let mut result = Vec::new();
    for user in found
        .iter()
        .filter(|u| visible(&current, u))
        .take(limit as usize)
    {
        result.push(danbooru_user(&state, db, user).await?);
    }
    json(result, &params.list.only)
}

#[derive(Debug, Default, Deserialize)]
struct Only {
    #[serde(default)]
    only: String,
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
    Query(params): Query<Only>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let id: i64 = id.parse().map_err(|_| AppError::NotFound)?;
    let user = users::by_id(db, id)
        .await?
        .filter(|u| visible(&current, u))
        .ok_or(AppError::NotFound)?;
    json(danbooru_user(&state, db, &user).await?, &params.only)
}

/// A wiki page as Danbooru describes it.
#[derive(Debug, Serialize)]
struct DanbooruWiki {
    id: i32,
    title: String,
    body: String,
    other_names: Vec<String>,
    is_locked: bool,
    is_deleted: bool,
    created_at: String,
    updated_at: String,
}

impl From<WikiPage> for DanbooruWiki {
    fn from(page: WikiPage) -> Self {
        Self {
            id: page.id,
            title: page.title,
            body: page.body,
            other_names: Vec::new(),
            is_locked: false,
            is_deleted: false,
            created_at: timestamp(page.created_at),
            updated_at: timestamp(page.updated_at),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct WikiParams {
    #[serde(rename = "search[title]", default)]
    title: String,
    #[serde(rename = "search[title_normalize]", default)]
    title_normalize: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn wiki_index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<WikiParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let limit = i64::from(params.list.limit(1000));
    let page: i64 = params.list.page.trim().parse().unwrap_or(1).clamp(1, 1000);
    let wanted = if params.title_normalize.trim().is_empty() {
        &params.title
    } else {
        &params.title_normalize
    };
    let title = normalize(wanted);
    let found: Vec<DanbooruWiki> = if !title.is_empty() && !title.contains('*') {
        // Without a wildcard, the title itself.
        wiki::by_title(db, &title)
            .await?
            .into_iter()
            .map(DanbooruWiki::from)
            .collect()
    } else {
        let summaries = wiki::list(db, &title, (page - 1) * limit, limit).await?;
        let mut pages = Vec::new();
        for summary in summaries {
            if let Some(page) = wiki::by_title(db, &summary.title).await? {
                pages.push(DanbooruWiki::from(page));
            }
        }
        pages
    };
    json(found, &params.list.only)
}

async fn wiki_show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(key): Path<String>,
    Query(params): Query<Only>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    // An id or a title; as on Danbooru, an all-digit key is an id.
    let page = match key.parse::<i32>() {
        Ok(id) => wiki::by_id(db, id).await?,
        Err(_) => wiki::by_title(db, &normalize(&key)).await?,
    };
    json(
        DanbooruWiki::from(page.ok_or(AppError::NotFound)?),
        &params.only,
    )
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    fn body(response: &crate::test_support::TestResponse) -> Value {
        serde_json::from_str(&response.body).unwrap_or_else(|_| panic!("{}", response.body))
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn profiles_and_users(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        session_for(&pool, "root", SystemRole::Admin).await;
        let id = upload(&app, &alice, 20, "cat").await;
        sqlx::query("INSERT INTO favorites (user_id, post_id) SELECT id, $1 FROM users WHERE name = 'alice'")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET settings = '{\"blacklist\": \"rating:e\\nspoilers\"}' WHERE name = 'alice'")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(
            app.get("/profile.json", None).await.status,
            StatusCode::UNAUTHORIZED
        );
        let me = body(&app.get("/profile.json", Some(&alice)).await);
        assert_eq!(me["name"], json!("alice"));
        assert_eq!(me["level"], json!(20));
        assert_eq!(me["blacklisted_tags"], json!("rating:e\nspoilers"));
        assert_eq!(me["post_upload_count"], json!(1));
        assert_eq!(me["favorite_count"], json!(1));
        assert_eq!(me["can_approve_posts"], json!(false));

        let listed = body(&app.get("/users.json?search[name_matches]=r*", None).await);
        assert_eq!(listed[0]["name"], json!("root"));
        assert_eq!(listed[0]["level"], json!(50));
        let alice_id = me["id"].as_i64().unwrap();
        let one = body(&app.get(&format!("/users/{alice_id}.json"), None).await);
        assert_eq!(one["name"], json!("alice"));
        let by_ids = body(
            &app.get(&format!("/users.json?search[id]={alice_id}"), None)
                .await,
        );
        assert_eq!(by_ids.as_array().unwrap().len(), 1);
        assert_eq!(
            app.get("/users/999.json", None).await.status,
            StatusCode::NOT_FOUND
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn wiki_pages(pool: PgPool) {
        let app = app(&pool).await;
        moekura_db::wiki::save(&pool, "long_hair", "Hair that is long.", None, None)
            .await
            .unwrap();
        moekura_db::wiki::save(&pool, "short_hair", "Not long.", None, None)
            .await
            .unwrap();
        let page = body(&app.get("/wiki_pages/Long%20Hair.json", None).await);
        assert_eq!(page["title"], json!("long_hair"));
        assert_eq!(page["body"], json!("Hair that is long."));
        let id = page["id"].as_i64().unwrap();
        let by_id = body(&app.get(&format!("/wiki_pages/{id}.json"), None).await);
        assert_eq!(by_id["title"], json!("long_hair"));
        let exact = body(
            &app.get("/wiki_pages.json?search[title]=short_hair", None)
                .await,
        );
        assert_eq!(exact[0]["body"], json!("Not long."));
        let pattern = body(&app.get("/wiki_pages.json?search[title]=*_hair", None).await);
        assert_eq!(pattern.as_array().unwrap().len(), 2);
        assert_eq!(
            app.get("/wiki_pages/nope.json", None).await.status,
            StatusCode::NOT_FOUND
        );
    }
}
