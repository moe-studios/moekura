//! The spam filter: comments, forum posts and messages that look like
//! spam (see [`moekura_core::spam::SpamFilter`]) are held, hidden, for the
//! staff to approve or reject at `/moderation/held`.

use axum::Router;
use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::held::{self, Held, Kind};
use moekura_db::mod_actions::{self, NewAction};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::url_value;

/// Held items per page.
const PAGE_SIZE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/moderation/held", get(index))
        .route("/moderation/held/{kind}/{id}/{action}", post(decide))
}

/// Why to hold `text`, written by `current`, for review; `None` to let it
/// through. Staff who review held writing are never held.
pub(crate) async fn check(
    state: &AppState,
    current: &CurrentUser,
    text: &str,
) -> Result<Option<String>, AppError> {
    let filter = state.site.get().settings.spam_filter.clone();
    if !filter.is_on(state.is_private()) || current.can(Permission::ModerateComments) {
        return Ok(None);
    }
    let Some(user) = current.user.as_ref() else {
        return Ok(None);
    };
    let days = (OffsetDateTime::now_utc() - user.created_at).whole_days();
    let repeats = held::repeats(state.db.primary(), user.id, text).await?;
    let hold = filter.check(text, days, repeats);
    if let Some(hold) = &hold {
        tracing::info!(user = user.id, %hold, "held as likely spam");
    }
    Ok(hold.map(|h| h.to_string()))
}

fn item_context(h: &Held) -> Value {
    // What it is, said by the template from the kind.
    let url = match h.kind.as_str() {
        "comment" => Some(format!("/comments/{}", h.id)),
        "forum_post" => Some(format!("/forum_posts/{}", h.id)),
        _ => None,
    };
    context! {
        kind => h.kind,
        id => h.id,
        post_id => h.parent_id,
        title => h.title,
        recipient => h.recipient_name,
        url => url.as_deref().map(url_value),
        creator => h.creator_name,
        reason => h.reason,
        html => Value::from_safe_string(markup::render(&h.body)),
        date => crate::dates::day(h.created_at),
        time => crate::dates::clock(h.created_at),
    }
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ModerateComments)?;
    let number = query.page.unwrap_or(1).clamp(1, 1000);
    let mut found = held::list(
        page.state().db.primary(),
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let more = found.len() > PAGE_SIZE as usize;
    found.truncate(PAGE_SIZE as usize);
    let settings = &page.state().site.get().settings.spam_filter;
    Ok(page.render(
        "moderation_held.html",
        context! {
            held => found.iter().map(item_context).collect::<Vec<_>>(),
            filter_on => settings.is_on(page.state().is_private()),
            previous_url => (number > 1).then(|| url_value(&format!("/moderation/held?page={}", number - 1))),
            next_url => more.then(|| url_value(&format!("/moderation/held?page={}", number + 1))),
        },
    ))
}

async fn decide(
    page: Page,
    jar: CookieJar,
    Path((kind, id, action)): Path<(String, i64, String)>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ModerateComments)?;
    let kind = Kind::parse(&kind).ok_or(AppError::NotFound)?;
    let approve = match action.as_str() {
        "approve" => true,
        "reject" => false,
        _ => return Err(AppError::NotFound),
    };
    let state = page.state();
    let db = state.db.primary();
    let item = held::get(db, kind, id).await?.ok_or(AppError::NotFound)?;
    let done = if approve {
        held::approve(db, kind, id).await?
    } else {
        held::reject(db, kind, id).await?
    };
    if !done {
        // Dealt with meanwhile.
        return Ok((
            flash::set(jar, Flash::SomeSkipped),
            Redirect::to("/moderation/held"),
        )
            .into_response());
    }
    if approve {
        published(state, &item, kind).await?;
    }
    let mut action = NewAction::new(
        page.current.user.as_ref().map(|u| u.id),
        if approve {
            ActionKind::HeldApprove
        } else {
            ActionKind::HeldReject
        },
    )
    .details(serde_json::json!({ "kind": kind.as_str(), "id": id, "reason": item.reason }));
    if let Some(creator) = item.creator_id {
        action = action.user(creator);
    }
    mod_actions::record(db, action).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/moderation/held"),
    )
        .into_response())
}

/// Does what posting an approved item would have done then: webhooks
/// and notifications.
async fn published(state: &AppState, item: &Held, kind: Kind) -> Result<(), AppError> {
    match kind {
        Kind::Comment => {
            let post = item.parent_id.unwrap_or_default();
            crate::comments::published(state, item.creator_id, item.id, post, &item.body).await;
        }
        Kind::ForumPost => {
            let topic_id = item.parent_id.unwrap_or_default();
            if let Some(topic) =
                moekura_db::forum::topic(state.db.primary(), None, topic_id).await?
            {
                crate::forum::post_published(state, item.creator_id, item.id, &topic, &item.body)
                    .await?;
            }
        }
        Kind::Dmail => {
            if let Some(to) = item.recipient_id {
                let title = item.title.as_deref().unwrap_or_default();
                crate::dmails::delivered(state, item.creator_id, to, item.id, title).await;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_db::settings;
    use serde_json::json;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn holds_and_reviews_likely_spam(pool: PgPool) {
        settings::set(&pool, "spam_filter", json!({ "words": ["casino"] }))
            .await
            .unwrap();
        let state = test_state(&pool).await;
        let app = TestApp::new(
            state.clone(),
            super::routes()
                .merge(crate::comments::routes())
                .merge(crate::dmails::routes())
                .merge(crate::forum::routes())
                .merge(crate::notifications::routes())
                .merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let staff = session_for(&pool, "staff", SystemRole::Moderator).await;
        let post = crate::danbooru::test_support::upload(&app, &staff, 20, "cat").await;

        // A new account's link is held; others' plain words aren't.
        let link = "body=Visit+https%3A%2F%2Fspam.example+%40bob";
        let held = app
            .post_form(&format!("/posts/{post}/comments"), Some(&alice), &[], link)
            .await;
        assert_eq!(held.status, StatusCode::SEE_OTHER, "{}", held.body);
        assert_eq!(
            held.location.as_deref(),
            Some(&*format!("/posts/{post}#comments"))
        );
        let ok = app
            .post_form(
                &format!("/posts/{post}/comments"),
                Some(&alice),
                &[],
                "body=Nice",
            )
            .await;
        assert!(ok.location.unwrap().contains("#comment-"));
        let comments: Vec<(String, bool)> =
            sqlx::query_as("SELECT body, is_deleted FROM comments ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert!(comments[0].1);
        assert!(!comments[1].1);
        // Nobody was told of it yet.
        assert!(
            !app.get("/notifications", Some(&bob))
                .await
                .body
                .contains("mentioned you")
        );

        // Spam words, in messages and forum topics.
        app.post_form(
            "/dmails",
            Some(&alice),
            &[],
            "to=bob&title=Hi&body=Best+CASINO",
        )
        .await;
        assert!(
            !app.get("/dmails", Some(&bob))
                .await
                .body
                .contains(">Hi</a>")
        );
        let general = moekura_db::forum::categories(&pool).await.unwrap()[0].id;
        let topic = app
            .post_form(
                "/forum_topics",
                Some(&alice),
                &[],
                &format!("category={general}&title=Deals&body=casino+deals"),
            )
            .await;
        assert_eq!(topic.location.as_deref(), Some("/forum_topics"));
        assert!(
            !app.get("/forum_topics", Some(&bob))
                .await
                .body
                .contains("Deals")
        );

        // Staff see all three; members can't.
        assert_eq!(
            app.get("/moderation/held", Some(&bob)).await.status,
            StatusCode::FORBIDDEN
        );
        let queue = app.get("/moderation/held", Some(&staff)).await.body;
        assert!(queue.contains("Held: links from a new account"), "{queue}");
        assert!(queue.contains("A message to bob: “Hi”"), "{queue}");
        assert!(queue.contains("A forum post in “Deals”"), "{queue}");

        // Approving lets it through, as if just posted.
        let comment: i64 = sqlx::query_scalar("SELECT min(id) FROM comments")
            .fetch_one(&pool)
            .await
            .unwrap();
        let approved = app
            .post_form(
                &format!("/moderation/held/comment/{comment}/approve"),
                Some(&staff),
                &[],
                "",
            )
            .await;
        assert_eq!(approved.status, StatusCode::SEE_OTHER);
        assert!(
            app.get("/notifications", Some(&bob))
                .await
                .body
                .contains("alice mentioned you")
        );
        let post_id: i64 = sqlx::query_scalar("SELECT id FROM forum_posts")
            .fetch_one(&pool)
            .await
            .unwrap();
        app.post_form(
            &format!("/moderation/held/forum_post/{post_id}/approve"),
            Some(&staff),
            &[],
            "",
        )
        .await;
        assert!(
            app.get("/forum_topics", Some(&bob))
                .await
                .body
                .contains("Deals")
        );

        // Rejected messages never arrive.
        let dmail: i64 = sqlx::query_scalar("SELECT id FROM dmails WHERE owner_id <> from_id")
            .fetch_one(&pool)
            .await
            .unwrap();
        app.post_form(
            &format!("/moderation/held/dmail/{dmail}/reject"),
            Some(&staff),
            &[],
            "",
        )
        .await;
        assert!(
            !app.get("/dmails", Some(&bob))
                .await
                .body
                .contains(">Hi</a>")
        );
        assert_eq!(
            app.get(&format!("/dmails/{dmail}"), Some(&bob))
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        assert!(
            app.get("/moderation/held", Some(&staff))
                .await
                .body
                .contains("Nothing is held")
        );
        // And twice is once.
        assert_eq!(
            app.post_form(
                &format!("/moderation/held/dmail/{dmail}/approve"),
                Some(&staff),
                &[],
                ""
            )
            .await
            .status,
            StatusCode::NOT_FOUND
        );

        // Staff are never held, and nobody is with the filter off.
        let staff_link = app
            .post_form(&format!("/posts/{post}/comments"), Some(&staff), &[], link)
            .await;
        assert!(staff_link.location.unwrap().contains("#comment-"));
        settings::set(&pool, "spam_filter", json!({ "mode": "off" }))
            .await
            .unwrap();
        state.site.reload(&pool).await.unwrap();
        let free = app
            .post_form(&format!("/posts/{post}/comments"), Some(&alice), &[], link)
            .await;
        assert!(free.location.unwrap().contains("#comment-"));
    }
}
