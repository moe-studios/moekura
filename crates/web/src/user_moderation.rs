//! The user moderation page: one user's record for staff, in one place.

use axum::Router;
use axum::extract::Path;
use axum::response::Response;
use axum::routing::get;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, Filter};
use moekura_db::user_record::{self, Tally};
use moekura_db::{bans, users};

use crate::AppState;
use crate::error::AppError;
use crate::pages::Page;
use crate::templates::url_value;

/// Entries of each list the page shows.
const RECENT: i64 = 20;

pub fn routes() -> Router<AppState> {
    Router::new().route("/moderation/users/{name}", get(page))
}

/// Whether `current` may see users' moderation pages.
pub(crate) fn may_view(current: &crate::auth::CurrentUser) -> bool {
    current.can(Permission::BanUsers) || current.can(Permission::ViewAuditLog)
}

/// The moderation page's URL for the user called `name`.
pub(crate) fn url(name: &str) -> String {
    format!(
        "/moderation/users/{}",
        url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>()
    )
}

fn tally_context(tally: &Tally, statuses: &[&str]) -> Value {
    let counts: Vec<Value> = statuses
        .iter()
        .map(|s| context! { status => s, count => tally.get(s) })
        .collect();
    context! { total => tally.total(), counts => counts }
}

fn log_url(key: &str, name: &str) -> Value {
    url_value(&format!(
        "/moderation/log?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair(key, name)
            .finish()
    ))
}

async fn page(page: Page, Path(name): Path<String>) -> Result<Response, AppError> {
    if !may_view(&page.current) {
        page.current.require(Permission::BanUsers)?;
    }
    let state = page.state();
    let db = state.db.primary();
    let user = users::by_name(db, &name).await?.ok_or(AppError::NotFound)?;
    let site = state.site.get();
    let account = user_record::account(db, user.id).await?;
    let ban_history = bans::for_user(db, user.id).await?;
    let banned = ban_history.iter().any(|b| b.active);
    let can_ban = crate::bans::may_ban(state, &page.current, &user);
    let read_log = page.current.can(Permission::ViewAuditLog);
    let (about, by) = if read_log {
        let about = Filter {
            user_id: Some(user.id),
            ..Filter::default()
        };
        let by = Filter {
            actor_id: Some(user.id),
            ..Filter::default()
        };
        (
            mod_actions::list(db, &about, RECENT).await?,
            mod_actions::list(db, &by, RECENT).await?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let uploads = user_record::uploads(db, user.id).await?;
    let deletions = user_record::deletions(db, user.id, RECENT).await?;
    let flags_received = user_record::flags_received(db, user.id).await?;
    let recent_flags = user_record::recent_flags_received(db, user.id, RECENT).await?;
    let flags_filed = user_record::flags_filed(db, user.id).await?;
    let reports = user_record::reports_received(db, user.id).await?;
    let hidden = user_record::hidden_comments(db, user.id, RECENT).await?;
    let flag_statuses = ["open", "upheld", "dismissed"];
    Ok(page.render(
        "moderation_user.html",
        context! {
            user => context! {
                name => user.name,
                profile_url => url_value(&format!(
                    "/users/{}",
                    url::form_urlencoded::byte_serialize(user.name.as_bytes()).collect::<String>()
                )),
                role => site.role(user.role_id).map(|r| r.name.clone()),
                status => user.status.as_str(),
                joined => user.created_at.date().to_string(),
                last_seen => user.last_seen_at.map(|t| t.date().to_string()),
                email => page.current.can(Permission::ManageUsers).then_some(user.email.clone()).flatten(),
                email_verified => user.email_verified_at.is_some(),
                two_factor => account.two_factor,
                promotion_blocked => account.auto_promotion_blocked,
            },
            bans => ban_history.iter().map(crate::bans::ban_context).collect::<Vec<_>>(),
            can_ban => can_ban,
            banned => banned,
            durations => crate::bans::durations(),
            read_log => read_log,
            about => about.iter().map(crate::moderation::entry_context).collect::<Vec<_>>(),
            about_url => log_url("user", &user.name),
            by => by.iter().map(crate::moderation::entry_context).collect::<Vec<_>>(),
            by_url => log_url("by", &user.name),
            uploads => tally_context(&uploads, &["active", "pending", "flagged", "deleted"]),
            uploads_url => url_value(&crate::templates::search_url(&format!("user:{}", user.name))),
            deletions => deletions.iter().map(|d| context! {
                post_id => d.post_id,
                rejected => d.action.as_deref() == Some("post.reject"),
                reason => d.reason,
                by => d.actor_name,
                when => d.deleted_at.map(|t| t.date().to_string()),
            }).collect::<Vec<_>>(),
            flags_received => tally_context(&flags_received, &flag_statuses),
            recent_flags => recent_flags.iter().map(|f| context! {
                post_id => f.post_id,
                by => f.creator_name,
                reason => f.reason,
                status => f.status,
                when => f.created_at.date().to_string(),
            }).collect::<Vec<_>>(),
            flags_filed => tally_context(&flags_filed, &flag_statuses),
            reports => tally_context(&reports, &flag_statuses),
            hidden => hidden.iter().map(|c| context! {
                id => c.id,
                post_id => c.post_id,
                body => c.body,
                reports => c.reports,
                when => c.created_at.date().to_string(),
            }).collect::<Vec<_>>(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::moderation::ActionKind;
    use moekura_core::permissions::SystemRole;
    use moekura_db::mod_actions::{self, NewAction};
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn staff_see_a_users_record(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes()
                .merge(crate::bans::routes())
                .merge(crate::users::routes())
                .merge(crate::moderation::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let janitor = session_for(&pool, "jan", SystemRole::Janitor).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let (alice_id, mod_id): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT id FROM users WHERE name = 'alice'), (SELECT id FROM users WHERE name = 'mod')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let post: i64 = sqlx::query_scalar(
            "INSERT INTO posts (rating, status, uploader_id) VALUES ('g', 'deleted', $1) RETURNING id",
        )
        .bind(alice_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        mod_actions::record(
            &pool,
            NewAction::new(Some(mod_id), ActionKind::PostDelete)
                .post(post)
                .reason("off-topic"),
        )
        .await
        .unwrap();

        for (who, status) in [
            (None, StatusCode::SEE_OTHER),
            (Some(&alice), StatusCode::FORBIDDEN),
            (Some(&janitor), StatusCode::FORBIDDEN),
        ] {
            assert_eq!(
                app.get("/moderation/users/alice", who.map(String::as_str))
                    .await
                    .status,
                status
            );
        }
        assert_eq!(
            app.get("/moderation/users/nobody", Some(&moderator))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        let response = app
            .post_form(
                "/users/alice/ban?back=%2Fmoderation%2Fusers%2Falice",
                Some(&moderator),
                &[],
                "reason=spam&days=7",
            )
            .await;
        assert_eq!(
            response.location.as_deref(),
            Some("/moderation/users/alice")
        );
        let page = app.get("/moderation/users/alice", Some(&moderator)).await;
        assert_eq!(page.status, StatusCode::OK);
        let body = page.body;
        assert!(body.contains("off-topic"), "the deletion reason: {body}");
        assert!(body.contains(&format!("href=\"/posts/{post}\"")), "{body}");
        assert!(body.contains("spam"), "the ban: {body}");
        assert!(body.contains("/users/alice/unban"), "{body}");
        assert!(body.contains("banned"), "the log entry: {body}");
        // The moderator's own page lists what they did.
        let own = app
            .get("/moderation/users/mod", Some(&moderator))
            .await
            .body;
        assert!(own.contains("deleted post"), "{own}");
        // Profiles and the log link to it.
        let profile = app.get("/users/alice", Some(&moderator)).await.body;
        assert!(
            profile.contains("href=\"/moderation/users/alice\""),
            "{profile}"
        );
        let member_view = app.get("/users/alice", Some(&alice)).await.body;
        assert!(!member_view.contains("/moderation/users/"));
        let log = app.get("/moderation/log", Some(&moderator)).await.body;
        assert!(log.contains("href=\"/moderation/users/alice\""), "{log}");
    }
}
