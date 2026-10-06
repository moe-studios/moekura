//! The user moderation page: one user's record for staff, in one place.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, Filter};
use moekura_db::user_notes::{self, UserNote};
use moekura_db::user_record::{self, Tally};
use moekura_db::{bans, post_batches, user_ips, users};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::url_value;

/// Entries of each list the page shows.
const RECENT: i64 = 20;

/// Deletions of all their uploads the page shows.
const BATCHES: i64 = 5;

/// Addresses, and other accounts on them, the page shows.
const ADDRESSES: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/moderation/users/{name}", get(page))
        .route("/users/{name}/notes", post(add_note))
        .route("/moderation/user-notes/{id}/delete", post(delete_note))
}

/// Whether `current` may see users' moderation pages, and staff notes.
pub(crate) fn may_view(current: &CurrentUser) -> bool {
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

/// The network an address is usually one of many in: its /24, or /64
/// for IPv6.
fn network_of(ip: std::net::IpAddr) -> ipnet::IpNet {
    let prefix = if ip.is_ipv4() { 24 } else { 64 };
    ipnet::IpNet::new(ip, prefix)
        .map(|net| net.trunc())
        .unwrap_or_else(|_| ip.into())
}

/// The bans page with its network form filled in with `network`.
fn ban_url(network: &str) -> Value {
    url_value(&format!(
        "/moderation/bans?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("network", network)
            .finish()
    ))
}

/// Staff notes about `user_id` for a page, if `current` may see them.
pub(crate) async fn notes(
    state: &AppState,
    current: &CurrentUser,
    user_id: i64,
) -> Result<Option<Vec<Value>>, AppError> {
    if !may_view(current) {
        return Ok(None);
    }
    let notes = user_notes::for_user(state.db.primary(), user_id).await?;
    Ok(Some(
        notes
            .iter()
            .map(|note| {
                context! {
                    id => note.id,
                    by => note.creator_name,
                    when => crate::dates::day(note.created_at),
                    html => Value::from_safe_string(markup::render(&note.body)),
                    can_delete => may_delete_note(current, note),
                }
            })
            .collect(),
    ))
}

/// Authors delete their own notes; those who can ban users, anyone's.
fn may_delete_note(current: &CurrentUser, note: &UserNote) -> bool {
    let own = current.user.as_ref().map(|u| u.id) == note.creator_id && note.creator_id.is_some();
    may_view(current) && (own || current.can(Permission::BanUsers))
}

#[derive(Debug, Default, Deserialize)]
struct Back {
    back: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NoteForm {
    #[serde(default)]
    body: String,
}

async fn add_note(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Query(back): Query<Back>,
    Form(form): Form<NoteForm>,
) -> Result<Response, AppError> {
    if !may_view(&page.current) {
        page.current.require(Permission::BanUsers)?;
    }
    let me = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = page.state().db.primary();
    let user = users::by_name(db, &name).await?.ok_or(AppError::NotFound)?;
    let body = form.body.replace("\r\n", "\n");
    let body = body.trim();
    if body.is_empty() || body.chars().count() > user_notes::MAX_LEN {
        return Err(AppError::BadRequest(format!(
            "A note has 1 to {} characters",
            user_notes::MAX_LEN
        )));
    }
    user_notes::create(db, user.id, me.id, body).await?;
    let to = crate::account::safe_next(back.back.as_deref()).to_owned();
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&to)).into_response())
}

async fn delete_note(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Query(back): Query<Back>,
) -> Result<Response, AppError> {
    if !may_view(&page.current) {
        page.current.require(Permission::BanUsers)?;
    }
    let db = page.state().db.primary();
    let note = user_notes::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !may_delete_note(&page.current, &note) {
        return Err(AppError::Forbidden);
    }
    user_notes::delete(db, id).await?;
    let to = crate::account::safe_next(back.back.as_deref()).to_owned();
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&to)).into_response())
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
    let delete_uploads = if crate::post_batches::may_delete_uploads(state, &page.current, &user) {
        Some(context! {
            count => post_batches::count_deletable(db, user.id).await?,
            open => post_batches::deletion_open(db, user.id).await?,
        })
    } else {
        None
    };
    let upload_deletions = post_batches::recent(db, "delete", Some(user.id), BATCHES).await?;
    let flags_received = user_record::flags_received(db, user.id).await?;
    let recent_flags = user_record::recent_flags_received(db, user.id, RECENT).await?;
    let flags_filed = user_record::flags_filed(db, user.id).await?;
    let reports = user_record::reports_received(db, user.id).await?;
    let hidden = user_record::hidden_comments(db, user.id, RECENT).await?;
    let disapprovals = Tally(moekura_db::disapprovals::counts_by(db, user.id).await?);
    let flag_statuses = ["open", "upheld", "dismissed"];
    let notes = notes(state, &page.current, user.id).await?;
    // Addresses are for those who can ban them: staff ranked above the
    // user, and above the other accounts listed.
    let (addresses, related) = if can_ban {
        (
            user_ips::for_user(db, user.id, ADDRESSES).await?,
            user_ips::related(db, user.id, page.current.role.rank, ADDRESSES).await?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
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
                joined => crate::dates::day(user.created_at),
                last_seen => user.last_seen_at.map(crate::dates::day),
                email => page.current.can(Permission::ManageUsers).then_some(user.email.clone()).flatten(),
                email_verified => user.email_verified_at.is_some(),
                two_factor => account.two_factor,
                promotion_blocked => account.auto_promotion_blocked,
            },
            bans => ban_history.iter().map(crate::bans::ban_context).collect::<Vec<_>>(),
            can_ban => can_ban,
            banned => banned,
            durations => crate::bans::durations(),
            notes => notes,
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
                when => d.deleted_at.map(crate::dates::day),
            }).collect::<Vec<_>>(),
            delete_uploads => delete_uploads,
            upload_deletions => upload_deletions.iter().map(crate::post_batches::batch_context).collect::<Vec<_>>(),
            show_addresses => can_ban,
            addresses => addresses.iter().map(|a| {
                let range = network_of(a.ip.addr());
                context! {
                    ip => a.ip.addr().to_string(),
                    first => crate::dates::day(a.first_seen_at),
                    last => crate::dates::day(a.last_seen_at),
                    ban_url => ban_url(&a.ip.addr().to_string()),
                    range => range.to_string(),
                    ban_range_url => ban_url(&range.to_string()),
                }
            }).collect::<Vec<_>>(),
            related => related.iter().map(|r| context! {
                name => r.name,
                url => url_value(&url(&r.name)),
                ip => r.ip.addr().to_string(),
                last => crate::dates::day(r.last_seen_at),
            }).collect::<Vec<_>>(),
            flags_received => tally_context(&flags_received, &flag_statuses),
            recent_flags => recent_flags.iter().map(|f| context! {
                post_id => f.post_id,
                by => f.creator_name,
                reason => f.reason,
                status => f.status,
                when => crate::dates::day(f.created_at),
            }).collect::<Vec<_>>(),
            flags_filed => tally_context(&flags_filed, &flag_statuses),
            disapprovals => tally_context(
                &disapprovals,
                &moekura_core::moderation::DisapprovalReason::ALL.map(|r| r.as_str()),
            ),
            reports => tally_context(&reports, &flag_statuses),
            hidden => hidden.iter().map(|c| context! {
                id => c.id,
                post_id => c.post_id,
                body => c.body,
                reports => c.reports,
                when => crate::dates::day(c.created_at),
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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn addresses_and_the_accounts_sharing_them(pool: PgPool) {
        let peer: std::net::SocketAddr = "203.0.113.7:4000".parse().unwrap();
        let app = TestApp::with_peer(
            test_state(&pool).await,
            super::routes().merge(crate::users::routes()),
            peer,
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        for who in [&alice, &bob] {
            let saved = app
                .post_form("/settings/theme", Some(who), &[], "mode=dark")
                .await;
            assert_eq!(saved.status, StatusCode::SEE_OTHER);
        }
        // Recorded in the background.
        for _ in 0..100 {
            let n: i64 = sqlx::query_scalar("SELECT count(*) FROM user_ips")
                .fetch_one(&pool)
                .await
                .unwrap();
            if n == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let page = app
            .get("/moderation/users/alice", Some(&moderator))
            .await
            .body;
        assert!(page.contains("<code>203.0.113.7</code>"), "{page}");
        assert!(page.contains("href=\"/moderation/users/bob\""), "{page}");
        assert!(
            page.contains("/moderation/bans?network=203.0.113.0%2F24"),
            "{page}"
        );
        // Not recorded when the site keeps none.
        moekura_db::settings::set(&pool, "ip_history_days", serde_json::json!(0))
            .await
            .unwrap();
        sqlx::query("DELETE FROM user_ips")
            .execute(&pool)
            .await
            .unwrap();
        let app = TestApp::with_peer(test_state(&pool).await, crate::users::routes(), peer);
        app.post_form("/settings/theme", Some(&alice), &[], "mode=light")
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM user_ips")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn staff_keep_notes_on_users(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::users::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        assert_eq!(
            app.post_form("/users/alice/notes", Some(&alice), &[], "body=hi")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let added = app
            .post_form(
                "/users/alice/notes?back=%2Fusers%2Falice",
                Some(&moderator),
                &[],
                "body=Warned+about+*spam*",
            )
            .await;
        assert_eq!(added.location.as_deref(), Some("/users/alice"));
        let profile = app.get("/users/alice", Some(&moderator)).await.body;
        assert!(profile.contains("Staff notes"), "{profile}");
        assert!(profile.contains("Warned about"), "{profile}");
        assert!(profile.contains("· mod"), "{profile}");
        let record = app.get("/moderation/users/alice", Some(&admin)).await.body;
        assert!(record.contains("Warned about"), "{record}");
        // Hidden from everyone else, the user included.
        let own = app.get("/users/alice", Some(&alice)).await.body;
        assert!(!own.contains("Warned about") && !own.contains("Staff notes"));

        let id: i64 = sqlx::query_scalar("SELECT id FROM user_notes")
            .fetch_one(&pool)
            .await
            .unwrap();
        let delete = format!("/moderation/user-notes/{id}/delete");
        assert_eq!(
            app.post(&delete, Some(&alice), &[]).await.status,
            StatusCode::FORBIDDEN
        );
        let deleted = app.post(&delete, Some(&admin), &[]).await;
        assert_eq!(deleted.status, StatusCode::SEE_OTHER);
        assert!(
            !app.get("/users/alice", Some(&moderator))
                .await
                .body
                .contains("Warned about")
        );
    }
}
