//! Requests to change tags: bulk update requests, and the voting and
//! discussion shared with alias and implication requests.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::bulk;
use moekura_core::jobs::ApplyBulkUpdate;
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::requests::{self, BulkRequest, Target};
use moekura_db::tag_relations::{self, Kind, Relation, Status};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::{search_url, url_value};

const PAGE_SIZE: i64 = 50;
const MAX_PAGE: i64 = 200;
const TITLE_MAX_LEN: usize = 200;
const SCRIPT_MAX_LEN: usize = 20_000;
const REASON_MAX_LEN: usize = 10_000;

/// Request statuses a list can be filtered by.
const STATUSES: [&str; 7] = [
    "pending",
    "applying",
    "applied",
    "failed",
    "rejected",
    "withdrawn",
    "approved",
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tags/requests", get(index).post(create))
        .route("/tags/requests/new", get(new_form))
        .route("/tags/requests/{id}", get(show))
        .route("/tags/requests/{id}/decide", post(decide))
        .route("/tags/requests/{id}/vote", post(vote_request))
        .route("/tags/requests/{id}/comments", post(comment_request))
        .route("/tags/aliases/{id}", get(show_alias))
        .route("/tags/implications/{id}", get(show_implication))
        .route("/tags/relations/{id}/vote", post(vote_relation))
        .route("/tags/relations/{id}/comments", post(comment_relation))
        .route("/tags/request-comments/{id}/delete", post(delete_comment))
}

pub(crate) fn relation_url(kind: Kind, id: i32) -> String {
    match kind {
        Kind::Alias => format!("/tags/aliases/{id}"),
        Kind::Implication => format!("/tags/implications/{id}"),
    }
}

fn target_url(state_db: &Target, relation_kind: Option<Kind>) -> String {
    match (state_db, relation_kind) {
        (Target::Relation(id), Some(kind)) => relation_url(kind, *id),
        (Target::Relation(id), None) => format!("/tags/aliases/{id}"),
        (Target::Request(id), _) => format!("/tags/requests/{id}"),
    }
}

/// Where a target's page is.
async fn url_of(state: &AppState, target: Target) -> Result<String, AppError> {
    Ok(match target {
        Target::Relation(id) => {
            let relation = tag_relations::by_id(state.db.primary(), id)
                .await?
                .ok_or(AppError::NotFound)?;
            relation_url(relation.kind, id)
        }
        Target::Request(_) => target_url(&target, None),
    })
}

/// Votes and discussion of a target, for templates.
async fn discussion(
    state: &AppState,
    current: &CurrentUser,
    target: Target,
    open: bool,
) -> Result<Value, AppError> {
    let db = state.db.primary();
    let me = current.user.as_ref().map(|u| u.id);
    let vote = match me {
        Some(me) => requests::vote_of(db, target, me).await?,
        None => 0,
    };
    let moderate = current.can(Permission::ManageTags);
    let comments: Vec<Value> = requests::comments(db, target, false)
        .await?
        .iter()
        .map(|c| {
            context! {
                id => c.id,
                author => c.creator_name,
                html => Value::from_safe_string(markup::render(&c.body)),
                date => c.created_at.date().to_string(),
                can_delete => current.is_logged_in() && (moderate || (me.is_some() && c.creator_id == me)),
            }
        })
        .collect();
    let (vote_url, comment_url) = match target {
        Target::Relation(id) => (
            format!("/tags/relations/{id}/vote"),
            format!("/tags/relations/{id}/comments"),
        ),
        Target::Request(id) => (
            format!("/tags/requests/{id}/vote"),
            format!("/tags/requests/{id}/comments"),
        ),
    };
    Ok(context! {
        vote => vote,
        can_vote => open && current.is_logged_in() && current.can(Permission::Vote),
        vote_url => url_value(&vote_url),
        comments => comments,
        can_comment => current.is_logged_in() && current.can(Permission::Comment),
        comment_url => url_value(&comment_url),
        max_len => crate::comments::MAX_LEN,
    })
}

#[derive(Debug, Deserialize)]
struct VoteForm {
    /// 1, -1, or 0 to take the vote back.
    score: i16,
}

/// Records `current`'s vote on a target, which must be open.
pub(crate) async fn vote(
    state: &AppState,
    current: &CurrentUser,
    target: Target,
    score: i16,
) -> Result<(), AppError> {
    current.require(Permission::Vote)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    if !(-1..=1).contains(&score) {
        return Err(AppError::BadRequest("A vote is 1, -1 or 0".into()));
    }
    let db = state.db.primary();
    let open = match target {
        Target::Relation(id) => {
            tag_relations::by_id(db, id)
                .await?
                .ok_or(AppError::NotFound)?
                .status
                == Status::Pending
        }
        Target::Request(id) => {
            requests::by_id(db, id)
                .await?
                .ok_or(AppError::NotFound)?
                .status
                == "pending"
        }
    };
    if !open {
        return Err(AppError::Unprocessable(
            "Only pending requests are voted on.".into(),
        ));
    }
    requests::vote(db, target, user.id, score).await?;
    Ok(())
}

/// Adds `current`'s comment to a target's discussion.
pub(crate) async fn comment(
    state: &AppState,
    current: &CurrentUser,
    target: Target,
    body: &str,
) -> Result<(), AppError> {
    current.require(Permission::Comment)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let body = crate::comments::clean_body(body)?;
    url_of(state, target).await?;
    state.rate_limits.check_comment(user.id).await?;
    requests::add_comment(state.db.primary(), target, user.id, &body).await?;
    Ok(())
}

async fn back(state: &AppState, jar: CookieJar, target: Target) -> Result<Response, AppError> {
    let url = url_of(state, target).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&url)).into_response())
}

async fn vote_request(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<VoteForm>,
) -> Result<Response, AppError> {
    vote(page.state(), &page.current, Target::Request(id), form.score).await?;
    back(page.state(), jar, Target::Request(id)).await
}

async fn vote_relation(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<VoteForm>,
) -> Result<Response, AppError> {
    vote(
        page.state(),
        &page.current,
        Target::Relation(id),
        form.score,
    )
    .await?;
    back(page.state(), jar, Target::Relation(id)).await
}

#[derive(Debug, Deserialize)]
struct CommentForm {
    body: String,
}

async fn comment_request(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<CommentForm>,
) -> Result<Response, AppError> {
    comment(page.state(), &page.current, Target::Request(id), &form.body).await?;
    back(page.state(), jar, Target::Request(id)).await
}

async fn comment_relation(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<CommentForm>,
) -> Result<Response, AppError> {
    comment(
        page.state(),
        &page.current,
        Target::Relation(id),
        &form.body,
    )
    .await?;
    back(page.state(), jar, Target::Relation(id)).await
}

async fn delete_comment(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = page.state().db.primary();
    let target = requests::comment_target(db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let whose = (!page.current.can(Permission::ManageTags)).then_some(user.id);
    if !requests::delete_comment(db, id, whose).await? {
        return Err(AppError::Forbidden);
    }
    back(page.state(), jar, target).await
}

async fn show_alias(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    show_relation(page, Kind::Alias, id).await
}

async fn show_implication(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    show_relation(page, Kind::Implication, id).await
}

async fn show_relation(page: Page, kind: Kind, id: i32) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let relation: Relation = tag_relations::by_id(page.state().db.primary(), id)
        .await?
        .filter(|r| r.kind == kind)
        .ok_or(AppError::NotFound)?;
    let pending = relation.status == Status::Pending;
    let manage = page.current.can(Permission::ManageTags);
    let me = page.current.user.as_ref().map(|u| u.id);
    let own = me.is_some() && relation.creator_id == me;
    let discussion = discussion(page.state(), &page.current, Target::Relation(id), pending).await?;
    let list = match kind {
        Kind::Alias => "/tags/aliases",
        Kind::Implication => "/tags/implications",
    };
    Ok(page.render(
        "tag_relation.html",
        context! {
            relation => context! {
                id => relation.id,
                kind => kind.as_str(),
                antecedent => relation.antecedent,
                antecedent_url => Value::from_safe_string(search_url(&relation.antecedent)),
                consequent => relation.consequent,
                consequent_url => Value::from_safe_string(search_url(&relation.consequent)),
                status => relation.status.as_str(),
                reason => relation.reason,
                creator => relation.creator_name,
                approver => relation.approver_name,
                score => relation.score,
                created => relation.created_at.date().to_string(),
            },
            list_url => Value::from_safe_string(list.to_owned()),
            can_approve => manage && pending,
            can_remove => (manage && relation.status == Status::Active) || (pending && (manage || own)),
            discussion => discussion,
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    status: String,
    page: Option<i64>,
}

fn summary(request: &BulkRequest) -> Value {
    context! {
        id => request.id,
        url => url_value(&format!("/tags/requests/{}", request.id)),
        title => request.title,
        status => request.status,
        creator => request.creator_name,
        approver => request.approver_name,
        score => request.score,
        created => request.created_at.date().to_string(),
    }
}

async fn index(page: Page, Query(query): Query<ListQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let status = STATUSES.iter().find(|s| **s == query.status).copied();
    let number = query.page.unwrap_or(1).clamp(1, MAX_PAGE);
    let mut found = requests::list(db, status, (number - 1) * PAGE_SIZE, PAGE_SIZE + 1).await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);
    let page_url = |n: i64| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("status", &query.status)
            .append_pair("page", &n.to_string())
            .finish();
        url_value(&format!("/tags/requests?{q}"))
    };
    Ok(page.render(
        "bulk_requests.html",
        context! {
            requests => found.iter().map(summary).collect::<Vec<_>>(),
            statuses => STATUSES,
            query => context! { status => query.status },
            can_request => page.current.is_logged_in() && page.current.can(Permission::EditPosts),
            previous_url => (number > 1).then(|| page_url(number - 1)),
            next_url => has_next.then(|| page_url(number + 1)),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct RequestForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    script: String,
    #[serde(default)]
    reason: String,
}

fn form_page(page: &Page, form: &RequestForm, error: Option<String>) -> Response {
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    page.render_with_status(
        status,
        "bulk_request_new.html",
        context! {
            form => context! { title => form.title, script => form.script, reason => form.reason },
            error => error,
        },
    )
}

async fn new_form(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::EditPosts)?;
    Ok(form_page(&page, &RequestForm::default(), None))
}

/// Checks and saves a bulk update request by `current`; returns its id.
pub(crate) async fn make_request(
    state: &AppState,
    current: &CurrentUser,
    title: &str,
    script: &str,
    reason: &str,
) -> Result<i32, AppError> {
    current.require(Permission::EditPosts)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let title = title.trim();
    let script = script.replace("\r\n", "\n");
    let reason = reason.replace("\r\n", "\n").trim().to_owned();
    let invalid = AppError::Unprocessable;
    if title.is_empty() || title.chars().count() > TITLE_MAX_LEN {
        return Err(invalid(format!(
            "Give a title of up to {TITLE_MAX_LEN} characters."
        )));
    }
    if script.len() > SCRIPT_MAX_LEN {
        return Err(invalid("The script is too long.".into()));
    }
    if reason.chars().count() > REASON_MAX_LEN {
        return Err(invalid("The reason is too long.".into()));
    }
    let commands = bulk::parse(&script).map_err(|e| invalid(format!("In the script, {e}.")))?;
    // Stored as the parser reads it, one command a line.
    let normalized: Vec<String> = commands.iter().map(bulk::Command::line).collect();
    let id = requests::create(
        state.db.primary(),
        user.id,
        title,
        &normalized.join("\n"),
        &reason,
    )
    .await?;
    tracing::info!(id, user = user.name, "bulk update requested");
    Ok(id)
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<RequestForm>,
) -> Result<Response, AppError> {
    match make_request(
        page.state(),
        &page.current,
        &form.title,
        &form.script,
        &form.reason,
    )
    .await
    {
        Ok(id) => Ok((
            flash::set(jar, Flash::Saved),
            Redirect::to(&format!("/tags/requests/{id}")),
        )
            .into_response()),
        Err(AppError::Unprocessable(message)) => Ok(form_page(&page, &form, Some(message))),
        Err(error) => Err(error),
    }
}

async fn show(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let request = requests::by_id(page.state().db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    let pending = request.status == "pending";
    let manage = page.current.can(Permission::ManageTags);
    let me = page.current.user.as_ref().map(|u| u.id);
    let commands: Vec<String> = request.script.lines().map(str::to_owned).collect();
    let discussion = discussion(page.state(), &page.current, Target::Request(id), pending).await?;
    Ok(page.render(
        "bulk_request.html",
        context! {
            request => summary(&request),
            commands => commands,
            reason => (!request.reason.is_empty())
                .then(|| Value::from_safe_string(markup::render(&request.reason))),
            error => request.error,
            can_decide => manage && pending,
            can_withdraw => pending && me.is_some() && request.creator_id == me,
            discussion => discussion,
        },
    ))
}

#[derive(Debug, Deserialize)]
struct DecideForm {
    /// `approve`, `reject` or `withdraw`.
    decision: String,
}

/// Approves (to apply it), rejects or withdraws request `id`.
pub(crate) async fn decide_request(
    state: &AppState,
    current: &CurrentUser,
    id: i32,
    decision: &str,
) -> Result<(), AppError> {
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = state.db.primary();
    let request = requests::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    let not_pending = || AppError::Unprocessable("The request was already decided.".into());
    match decision {
        "approve" | "reject" => {
            current.require(Permission::ManageTags)?;
            let approve = decision == "approve";
            let mut tx = db.begin().await?;
            let to = if approve { "applying" } else { "rejected" };
            if !requests::set_status(&mut *tx, id, "pending", to, Some(user.id)).await? {
                return Err(not_pending());
            }
            if approve {
                moekura_db::jobs::enqueue(&mut tx, &ApplyBulkUpdate { request_id: id }).await?;
            }
            let kind = if approve {
                ActionKind::BulkUpdateApprove
            } else {
                ActionKind::BulkUpdateReject
            };
            mod_actions::record(
                &mut *tx,
                NewAction::new(Some(user.id), kind)
                    .details(serde_json::json!({ "request_id": id, "title": request.title })),
            )
            .await?;
            tx.commit().await?;
        }
        "withdraw" => {
            if request.creator_id != Some(user.id) {
                return Err(AppError::Forbidden);
            }
            if !requests::set_status(db, id, "pending", "withdrawn", None).await? {
                return Err(not_pending());
            }
        }
        _ => return Err(AppError::BadRequest("Unknown decision".into())),
    }
    tracing::info!(
        id,
        decision,
        user = user.name,
        "bulk update request decided"
    );
    Ok(())
}

async fn decide(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<DecideForm>,
) -> Result<Response, AppError> {
    decide_request(page.state(), &page.current, id, &form.decision).await?;
    back(page.state(), jar, Target::Request(id)).await
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(
            test_state(pool).await,
            routes().merge(crate::tag_relations::routes()),
        )
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn bulk_requests_are_voted_discussed_and_decided(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;

        let bad = app
            .post_form(
                "/tags/requests",
                Some(&alice),
                &[],
                "title=Cats&script=alias+kitty+cat",
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            bad.body.contains("line 1: expected `-&gt;`"),
            "{}",
            bad.body
        );
        assert!(
            bad.body.contains(">alias kitty cat</textarea>"),
            "keeps the script"
        );

        let made = app
            .post_form(
                "/tags/requests",
                Some(&alice),
                &[],
                "title=Cats&script=%23+tidy%0D%0AAlias+Kitty+-%3E+cat%0D%0Aimply+cat+-%3E+animal&reason=Why+not",
            )
            .await;
        assert_eq!(made.status, StatusCode::SEE_OTHER, "{}", made.body);
        let url = made.location.unwrap();
        let id: i32 = url.rsplit('/').next().unwrap().parse().unwrap();
        let shown = app.get(&url, None).await;
        assert!(
            shown
                .body
                .contains("alias kitty -&gt; cat\nimply cat -&gt; animal"),
            "{}",
            shown.body
        );

        app.post_form(&format!("{url}/vote"), Some(&bob), &[], "score=1")
            .await;
        app.post_form(
            &format!("{url}/comments"),
            Some(&bob),
            &[],
            "body=Good+[b]idea[/b]",
        )
        .await;
        let shown = app.get(&url, Some(&bob)).await;
        assert!(shown.body.contains("class=\"score\">1<"), "{}", shown.body);
        assert!(shown.body.contains("Good <strong>idea</strong>"));
        assert!(!shown.body.contains("Approve and apply"));

        assert_eq!(
            app.post_form(
                &format!("{url}/decide"),
                Some(&bob),
                &[],
                "decision=approve"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.post_form(
                &format!("{url}/decide"),
                Some(&bob),
                &[],
                "decision=withdraw"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        let approved = app
            .post_form(
                &format!("{url}/decide"),
                Some(&jan),
                &[],
                "decision=approve",
            )
            .await;
        assert_eq!(approved.status, StatusCode::SEE_OTHER, "{}", approved.body);
        let request = requests::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(request.status, "applying");
        let jobs: i64 =
            sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'tags.bulk_update'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(jobs, 1);
        // Decided: no more votes, and it can't be decided twice.
        assert_eq!(
            app.post_form(&format!("{url}/vote"), Some(&alice), &[], "score=-1")
                .await
                .status,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(
            app.post_form(&format!("{url}/decide"), Some(&jan), &[], "decision=reject")
                .await
                .status,
            StatusCode::UNPROCESSABLE_ENTITY
        );

        let other = app
            .post_form(
                "/tags/requests",
                Some(&alice),
                &[],
                "title=Dogs&script=imply+dog+-%3E+animal",
            )
            .await
            .location
            .unwrap();
        app.post_form(
            &format!("{other}/decide"),
            Some(&alice),
            &[],
            "decision=withdraw",
        )
        .await;
        let list = app.get("/tags/requests?status=withdrawn", None).await;
        assert!(
            list.body.contains("Dogs") && !list.body.contains(">Cats<"),
            "{}",
            list.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn relation_requests_are_voted_and_discussed(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        app.post_form(
            "/tags/aliases",
            Some(&alice),
            &[],
            "antecedent=kitty&consequent=cat",
        )
        .await;
        let id: i32 = sqlx::query_scalar("SELECT id FROM tag_relations")
            .fetch_one(&pool)
            .await
            .unwrap();
        let list = app.get("/tags/aliases", None).await;
        assert!(
            list.body.contains(&format!("href=\"/tags/aliases/{id}\"")),
            "{}",
            list.body
        );

        let vote = format!("/tags/relations/{id}/vote");
        app.post_form(&vote, Some(&bob), &[], "score=-1").await;
        app.post_form(&vote, Some(&jan), &[], "score=-1").await;
        app.post_form(
            &format!("/tags/relations/{id}/comments"),
            Some(&bob),
            &[],
            "body=No",
        )
        .await;
        let page = app.get(&format!("/tags/aliases/{id}"), Some(&alice)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("class=\"score\">-2<"), "{}", page.body);
        assert!(page.body.contains("<p>No</p>"));
        // An implication's page doesn't show an alias.
        assert_eq!(
            app.get(&format!("/tags/implications/{id}"), None)
                .await
                .status,
            StatusCode::NOT_FOUND
        );

        let comment: i64 = sqlx::query_scalar("SELECT id FROM request_comments")
            .fetch_one(&pool)
            .await
            .unwrap();
        let delete = format!("/tags/request-comments/{comment}/delete");
        assert_eq!(
            app.post(&delete, Some(&alice), &[]).await.status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.post(&delete, Some(&jan), &[]).await.status,
            StatusCode::SEE_OTHER
        );
        assert!(
            !app.get(&format!("/tags/aliases/{id}"), None)
                .await
                .body
                .contains("<p>No</p>")
        );
    }
}
