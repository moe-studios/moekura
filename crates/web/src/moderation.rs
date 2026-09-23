//! Moderation pages.

use axum::Router;
use axum::extract::Query;
use axum::response::Response;
use axum::routing::get;
use minijinja::{Value, context};
use serde::Deserialize;
use uwuu_core::moderation::ActionKind;
use uwuu_core::permissions::Permission;
use uwuu_db::mod_actions::{self, Entry, Filter};
use uwuu_db::users;

use crate::AppState;
use crate::error::AppError;
use crate::pages::Page;
use crate::templates::url_value;

/// Log entries per page.
const LOG_PAGE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new().route("/moderation/log", get(log))
}

#[derive(Debug, Default, Deserialize)]
struct LogQuery {
    #[serde(default)]
    action: String,
    /// Moderator name.
    #[serde(default)]
    by: String,
    #[serde(default)]
    post: String,
    before: Option<i64>,
}

fn entry_context(entry: &Entry) -> Value {
    let kind = ActionKind::parse(&entry.action);
    let details: Vec<Value> = entry
        .details
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    let text = match value {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    context! { key => key, value => text }
                })
                .collect()
        })
        .unwrap_or_default();
    context! {
        id => entry.id,
        when => entry.created_at.date().to_string(),
        time => format!("{:02}:{:02}", entry.created_at.hour(), entry.created_at.minute()),
        actor => entry.actor_name,
        label => kind.map_or_else(|| entry.action.clone(), |k| k.label().to_owned()),
        post_id => entry.post_id,
        user => entry.user_name,
        reason => entry.reason,
        details => details,
    }
}

async fn log(page: Page, Query(query): Query<LogQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewAuditLog)?;
    let db = page.state().db.read();
    let actor_id = match query.by.trim() {
        "" => None,
        name => Some(users::by_name(db, name).await?.map_or(-1, |user| user.id)),
    };
    let filter = Filter {
        action: ActionKind::parse(&query.action),
        actor_id,
        post_id: query.post.trim().trim_start_matches('#').parse().ok(),
        user_id: None,
        before: query.before,
    };
    let entries = mod_actions::list(db, &filter, LOG_PAGE).await?;
    let older = (entries.len() == LOG_PAGE as usize)
        .then(|| entries.last().map(|e| e.id))
        .flatten()
        .map(|id| {
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("action", &query.action)
                .append_pair("by", &query.by)
                .append_pair("post", &query.post)
                .append_pair("before", &id.to_string())
                .finish();
            url_value(&format!("/moderation/log?{q}"))
        });
    Ok(page.render(
        "moderation_log.html",
        context! {
            entries => entries.iter().map(entry_context).collect::<Vec<_>>(),
            actions => ActionKind::ALL.iter().map(|k| context! { name => k.as_str(), label => k.label() }).collect::<Vec<_>>(),
            query => context! { action => query.action, by => query.by, post => query.post },
            older_url => older,
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use sqlx::PgPool;
    use uwuu_core::moderation::ActionKind;
    use uwuu_core::permissions::SystemRole;
    use uwuu_db::mod_actions::{self, NewAction};

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn the_log_is_for_moderators(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let member = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        mod_actions::record(
            &pool,
            NewAction::new(None, ActionKind::PostDelete)
                .post(12)
                .reason("off-topic")
                .details(serde_json::json!({ "tags": "a b" })),
        )
        .await
        .unwrap();

        assert_eq!(
            app.get("/moderation/log", Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
        let page = app.get("/moderation/log", Some(&moderator)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("deleted post"), "{}", page.body);
        assert!(page.body.contains("href=\"/posts/12\""));
        assert!(page.body.contains("off-topic"));
        let filtered = app
            .get("/moderation/log?action=user.ban", Some(&moderator))
            .await;
        assert!(!filtered.body.contains("off-topic"));
        let by_nobody = app.get("/moderation/log?by=nobody", Some(&moderator)).await;
        assert!(!by_nobody.body.contains("off-topic"));
    }
}
