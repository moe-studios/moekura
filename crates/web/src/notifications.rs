//! Notifications: `@name` mentions and quote-replies in comments, forum
//! posts and request discussions; new messages; new posts in forum topics
//! one posted in; decisions on requests one made or voted on. Listed at
//! `/notifications`, counted in the header, and emailed to those who
//! asked.

use axum::Form;
use axum::Router;
use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::context;
use moekura_core::markup;
use moekura_core::user_settings::UserSettings;
use moekura_db::notifications::{self, Kind};
use moekura_db::users;
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::url_value;

/// Notifications per page.
const PAGE_SIZE: i64 = 50;
/// Most users one text can notify by mentioning them.
const MAX_MENTIONS: usize = 20;
/// Days read notifications are kept.
pub const KEEP_DAYS: i32 = 90;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/notifications", get(index))
        .route("/notifications/read_all", post(read_all))
        .route("/notifications/{id}", get(open))
}

/// What each kind says, after the actor's name.
fn describe(kind: &str) -> &'static str {
    match kind {
        "mention" => "mentioned you in",
        "reply" => "replied to you in",
        "message" => "sent you a message:",
        "forum" => "posted in",
        _ => "decided on",
    }
}

/// Notifies `user_ids` of something `actor` did, leaving out the actor
/// and anyone who blocked them, and emailing those who asked. Failures
/// are only logged: whatever happened has happened.
pub(crate) async fn notify(
    state: &AppState,
    user_ids: &[i64],
    kind: Kind,
    actor: Option<i64>,
    subject: &str,
    url: &str,
) {
    let done = async {
        let db = state.db.primary();
        let mut ids: Vec<i64> = user_ids
            .iter()
            .copied()
            .filter(|id| Some(*id) != actor)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        if let Some(actor) = actor {
            let blocking = moekura_db::dmails::blocking(db, actor, &ids).await?;
            ids.retain(|id| !blocking.contains(id));
        }
        if ids.is_empty() {
            return Ok::<_, sqlx::Error>(());
        }
        notifications::create(db, &ids, kind, actor, subject, url).await?;
        if state.config.mail.is_enabled() {
            let actor_name = match actor {
                Some(id) => users::by_id(db, id).await?.map(|u| u.name),
                None => None,
            };
            let site = state.site.get().settings.site_name.clone();
            let link = crate::api::absolute_url(state, url);
            for user in users::by_ids(db, &ids).await? {
                let (Some(email), Some(_)) = (&user.email, user.email_verified_at) else {
                    continue;
                };
                if !UserSettings::from_json(&user.settings).email_notifications {
                    continue;
                }
                let who = actor_name.as_deref().unwrap_or("Someone");
                let line = format!("{who} {} {subject}", describe(kind.as_str()));
                let mail = moekura_core::jobs::SendMail {
                    to: email.clone(),
                    subject: format!("{site}: {line}"),
                    body: format!(
                        "{line}\n\n{link}\n\nTurn these emails off under Settings on {site}.\n"
                    ),
                };
                let mut conn = db.acquire().await?;
                moekura_db::jobs::enqueue(&mut conn, &mail).await?;
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = done {
        tracing::warn!(%error, "could not notify");
    }
}

/// Notifies those `text` mentions (`@name`) or quotes, as written by
/// `actor_id` at `url`, and then `also` (with their own kind) unless
/// already notified.
pub(crate) async fn notify_text(
    state: &AppState,
    actor_id: Option<i64>,
    text: &str,
    subject: &str,
    url: &str,
    also: (&[i64], Kind),
) {
    let db = state.db.primary();
    let mut done: Vec<i64> = Vec::new();
    let named = async |names: Vec<String>| -> Vec<i64> {
        let mut ids = Vec::new();
        for name in names.into_iter().take(MAX_MENTIONS) {
            if let Ok(Some(user)) = users::by_name(db, &name).await {
                ids.push(user.id);
            }
        }
        ids
    };
    let mentioned = named(markup::mentions(text)).await;
    notify(state, &mentioned, Kind::Mention, actor_id, subject, url).await;
    done.extend(&mentioned);
    let quoted: Vec<i64> = named(markup::quoted_authors(text))
        .await
        .into_iter()
        .filter(|id| !done.contains(id))
        .collect();
    notify(state, &quoted, Kind::Reply, actor_id, subject, url).await;
    done.extend(&quoted);
    let (others, kind) = also;
    let others: Vec<i64> = others
        .iter()
        .copied()
        .filter(|id| !done.contains(id))
        .collect();
    notify(state, &others, kind, actor_id, subject, url).await;
}

/// After a comment on post `post_id`: those it mentions or quotes.
pub(crate) async fn comment_posted(
    state: &AppState,
    actor: Option<i64>,
    comment_id: i64,
    post_id: i64,
    body: &str,
) {
    let subject = format!("a comment on post #{post_id}");
    let url = format!("/comments/{comment_id}");
    notify_text(state, actor, body, &subject, &url, (&[], Kind::Reply)).await;
}

/// After a decision on a request: its creator and its voters.
pub(crate) async fn request_decided(
    state: &AppState,
    actor: &CurrentUser,
    target: moekura_db::requests::Target,
    creator: Option<i64>,
    subject: &str,
    url: &str,
) {
    let mut people = match moekura_db::requests::voters(state.db.primary(), target).await {
        Ok(voters) => voters,
        Err(error) => {
            tracing::warn!(%error, "could not find a request's voters");
            Vec::new()
        }
    };
    people.extend(creator);
    let actor = actor.user.as_ref().map(|u| u.id);
    notify(state, &people, Kind::Request, actor, subject, url).await;
}

/// Forgets read notifications older than [`KEEP_DAYS`].
pub async fn prune(state: &AppState) {
    if let Err(error) = notifications::prune(state.db.primary(), KEEP_DAYS).await {
        tracing::warn!(%error, "could not prune old notifications");
    }
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    unread: String,
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    let me = page
        .current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)?;
    let number = query.page.unwrap_or(1).max(1);
    let unread = query.unread == "1";
    let mut found = notifications::list(
        page.state().db.primary(),
        me,
        unread,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let more = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);
    let list_url = |n: i64| {
        url_value(&format!(
            "/notifications?page={n}{}",
            if unread { "&unread=1" } else { "" }
        ))
    };
    Ok(page.render(
        "notifications.html",
        context! {
            unread => unread,
            notifications => found.iter().map(|n| context! {
                id => n.id,
                actor => n.actor_name,
                says => describe(&n.kind),
                subject => n.subject,
                read => n.is_read,
                date => crate::dates::day(n.created_at),
                time => crate::dates::clock(n.created_at),
            }).collect::<Vec<_>>(),
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => more.then(|| list_url(number + 1)),
        },
    ))
}

/// Marks a notification read and goes where it leads.
async fn open(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let me = page
        .current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)?;
    let url = notifications::read(page.state().db.primary(), me, id)
        .await?
        .filter(|u| u.starts_with('/') && !u.starts_with("//"))
        .ok_or(AppError::NotFound)?;
    Ok(Redirect::to(&url).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct Empty {}

async fn read_all(page: Page, jar: CookieJar, Form(_): Form<Empty>) -> Result<Response, AppError> {
    let me = page
        .current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)?;
    notifications::read_all(page.state().db.primary(), me).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/notifications"),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn mentions_replies_messages_and_forum(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes()
                .merge(crate::comments::routes())
                .merge(crate::dmails::routes())
                .merge(crate::forum::routes())
                .merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let carol = session_for(&pool, "carol", SystemRole::Member).await;
        let post = crate::danbooru::test_support::upload(&app, &alice, 20, "cat").await;

        let commented = app
            .post_form(
                &format!("/posts/{post}/comments"),
                Some(&alice),
                &[],
                "body=Look+%40bob%2C+and+%40alice",
            )
            .await;
        assert_eq!(
            commented.status,
            StatusCode::SEE_OTHER,
            "{}",
            commented.body
        );
        let list = app.get("/notifications", Some(&bob)).await.body;
        assert!(
            list.contains("alice mentioned you in a comment on post"),
            "{list}"
        );
        assert!(
            app.get("/", Some(&bob))
                .await
                .body
                .contains("Notifications, 1 unread")
        );
        // Not for mentioning yourself.
        assert!(
            !app.get("/notifications", Some(&alice))
                .await
                .body
                .contains("mentioned you")
        );

        // Quoting is replying; carol blocked alice, so hears nothing.
        app.post_form("/users/alice/block", Some(&carol), &[], "blocked=1")
            .await;
        let reply = "body=%5Bquote%5D%0Abob+said%3A%0A%0Ahi%0A%5B%2Fquote%5D%0A%0AHello+%40carol";
        app.post_form(&format!("/posts/{post}/comments"), Some(&alice), &[], reply)
            .await;
        let bobs = app.get("/notifications?unread=1", Some(&bob)).await.body;
        assert!(bobs.contains("alice replied to you"), "{bobs}");
        assert!(
            !app.get("/notifications", Some(&carol))
                .await
                .body
                .contains("alice")
        );

        // Messages and forum topics.
        app.post_form("/dmails", Some(&alice), &[], "to=bob&title=Hey&body=Hi")
            .await;
        assert!(
            app.get("/notifications", Some(&bob))
                .await
                .body
                .contains("sent you a message")
        );
        let general = moekura_db::forum::categories(&pool).await.unwrap()[0].id;
        let topic = app
            .post_form(
                "/forum_topics",
                Some(&bob),
                &[],
                &format!("category={general}&title=Cats&body=Discuss"),
            )
            .await
            .location
            .unwrap();
        app.post_form(&format!("{topic}/posts"), Some(&alice), &[], "body=Agreed")
            .await;
        let found = app.get("/notifications", Some(&bob)).await.body;
        assert!(found.contains("alice posted in the forum topic"), "{found}");

        // Opening one marks it read and goes there.
        let first = moekura_db::notifications::list(
            &pool,
            moekura_db::users::by_name(&pool, "bob")
                .await
                .unwrap()
                .unwrap()
                .id,
            true,
            0,
            1,
        )
        .await
        .unwrap()
        .remove(0);
        let opened = app
            .get(&format!("/notifications/{}", first.id), Some(&bob))
            .await;
        assert_eq!(opened.location.as_deref(), Some(first.url.as_str()));
        assert_eq!(
            app.get(&format!("/notifications/{}", first.id), Some(&alice))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }
}
