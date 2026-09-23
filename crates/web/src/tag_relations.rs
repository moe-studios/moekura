//! Pages for tag alias and implication requests.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use serde::Deserialize;
use uwuu_core::moderation::ActionKind;
use uwuu_core::permissions::Permission;
use uwuu_core::tags::TagName;
use uwuu_db::mod_actions::{self, NewAction};
use uwuu_db::tag_relations::{self, Kind, NewRequest, Relation, RelationError, Status};

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::{search_url, url_value};

const PAGE_SIZE: i64 = 50;
const MAX_PAGE: i64 = 200;
const REASON_MAX_LEN: usize = 2000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tags/aliases", get(aliases).post(request_alias))
        .route(
            "/tags/implications",
            get(implications).post(request_implication),
        )
        .route("/tags/aliases/{id}/{action}", post(act))
        .route("/tags/implications/{id}/{action}", post(act))
}

fn path(kind: Kind) -> &'static str {
    match kind {
        Kind::Alias => "/tags/aliases",
        Kind::Implication => "/tags/implications",
    }
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: String,
    page: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct RequestForm {
    #[serde(default)]
    antecedent: String,
    #[serde(default)]
    consequent: String,
    #[serde(default)]
    reason: String,
}

async fn aliases(page: Page, Query(query): Query<ListQuery>) -> Result<Response, AppError> {
    index(page, Kind::Alias, query, None, StatusCode::OK).await
}

async fn implications(page: Page, Query(query): Query<ListQuery>) -> Result<Response, AppError> {
    index(page, Kind::Implication, query, None, StatusCode::OK).await
}

async fn request_alias(
    page: Page,
    jar: CookieJar,
    Form(form): Form<RequestForm>,
) -> Result<Response, AppError> {
    create(page, jar, Kind::Alias, form).await
}

async fn request_implication(
    page: Page,
    jar: CookieJar,
    Form(form): Form<RequestForm>,
) -> Result<Response, AppError> {
    create(page, jar, Kind::Implication, form).await
}

/// Renders the list, with `failed` (a rejected form and its error) shown
/// in the request form.
async fn index(
    page: Page,
    kind: Kind,
    query: ListQuery,
    failed: Option<(RequestForm, String)>,
    status: StatusCode,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().db.read();
    let filter: Option<Status> = query.status.parse().ok();
    let number = query.page.unwrap_or(1).clamp(1, MAX_PAGE);
    let name = uwuu_core::tags::normalize(&query.name);
    let mut found = tag_relations::list(
        db,
        kind,
        filter,
        &name,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);

    let manage = page.current.can(Permission::ManageTags);
    let me = page.current.user.as_ref().map(|u| u.id);
    let rows: Vec<Value> = found.iter().map(|r| row_context(r, manage, me)).collect();
    let page_url = |n: i64| {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", &query.name)
            .append_pair("status", &query.status)
            .append_pair("page", &n.to_string())
            .finish();
        url_value(&format!("{}?{query}", path(kind)))
    };
    let (form, error) = failed.unzip();
    let form = form.unwrap_or_default();
    Ok(page.render_with_status(
        status,
        "tag_relations.html",
        context! {
            kind => kind.as_str(),
            path => Value::from_safe_string(path(kind).to_owned()),
            title => match kind { Kind::Alias => "Tag aliases", Kind::Implication => "Tag implications" },
            relations => rows,
            statuses => Status::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            query => context! { name => query.name, status => query.status },
            can_request => page.current.can(Permission::EditPosts),
            can_manage => manage,
            form => context! { antecedent => form.antecedent, consequent => form.consequent, reason => form.reason },
            error => error,
            previous_url => (number > 1).then(|| page_url(number - 1)),
            next_url => has_next.then(|| page_url(number + 1)),
        },
    ))
}

fn row_context(relation: &Relation, manage: bool, me: Option<i64>) -> Value {
    let pending = relation.status == Status::Pending;
    let own = me.is_some() && relation.creator_id == me;
    context! {
        id => relation.id,
        antecedent => relation.antecedent,
        antecedent_url => Value::from_safe_string(search_url(&relation.antecedent)),
        consequent => relation.consequent,
        consequent_url => Value::from_safe_string(search_url(&relation.consequent)),
        status => relation.status.as_str(),
        reason => relation.reason,
        creator => relation.creator_name,
        approver => relation.approver_name,
        created => relation.created_at.date().to_string(),
        can_approve => manage && pending,
        can_remove => (manage && relation.status == Status::Active) || (pending && (manage || own)),
    }
}

async fn create(
    page: Page,
    jar: CookieJar,
    kind: Kind,
    form: RequestForm,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditPosts)?;
    let Some(user) = page.current.user.clone() else {
        return Err(AppError::Unauthorized);
    };
    let reject = |page, form, message: String| {
        index(
            page,
            kind,
            ListQuery::default(),
            Some((form, message)),
            StatusCode::UNPROCESSABLE_ENTITY,
        )
    };
    let parse =
        |label: &str, raw: &str| TagName::parse(raw).map_err(|e| format!("The {label} tag {e}."));
    let names = parse("first", &form.antecedent)
        .and_then(|a| Ok((a, parse("second", &form.consequent)?)))
        .and_then(|(a, c)| {
            if a == c {
                Err("Pick two different tags.".to_owned())
            } else if form.reason.chars().count() > REASON_MAX_LEN {
                Err(format!(
                    "The reason may be at most {REASON_MAX_LEN} characters."
                ))
            } else {
                Ok((a, c))
            }
        });
    let (antecedent, consequent) = match names {
        Ok(names) => names,
        Err(message) => return reject(page, form, message).await,
    };

    let db = page.state().db.primary();
    let request = NewRequest {
        kind,
        antecedent: antecedent.as_str(),
        consequent: consequent.as_str(),
        reason: form.reason.trim(),
        creator_id: Some(user.id),
    };
    let result = match tag_relations::request(db, request).await {
        // Tag managers' own requests take effect at once.
        Ok(id) if page.current.can(Permission::ManageTags) => {
            match tag_relations::approve(db, id, user.id).await {
                Ok(()) => {
                    audit(db, user.id, ActionKind::TagRelationApprove, id).await?;
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        other => other.map(|_| ()),
    };
    match result {
        Ok(()) => {
            tracing::info!(%kind, %antecedent, %consequent, user = user.name, "tag relation requested");
            Ok((flash::set(jar, Flash::Saved), Redirect::to(path(kind))).into_response())
        }
        Err(RelationError::Rule(rule)) => reject(page, form, rule.to_string()).await,
        Err(RelationError::Db(e)) => Err(e.into()),
    }
}

async fn act(
    page: Page,
    jar: CookieJar,
    Path((id, action)): Path<(i32, String)>,
) -> Result<Response, AppError> {
    let Some(user) = page.current.user.clone() else {
        return Err(AppError::Unauthorized);
    };
    let db = page.state().db.primary();
    let relation = tag_relations::by_id(db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    let manage = page.current.can(Permission::ManageTags);
    let allowed = match action.as_str() {
        "approve" | "reject" => manage,
        "remove" => {
            manage || (relation.status == Status::Pending && relation.creator_id == Some(user.id))
        }
        _ => return Err(AppError::NotFound),
    };
    if !allowed {
        return Err(AppError::Forbidden);
    }
    let result = match action.as_str() {
        "approve" => tag_relations::approve(db, id, user.id).await,
        "reject" => tag_relations::reject(db, id, user.id).await,
        _ => tag_relations::remove(db, id, user.id)
            .await
            .map(|_| ())
            .map_err(RelationError::Db),
    };
    match result {
        Ok(()) => {
            if manage {
                let kind = match action.as_str() {
                    "approve" => ActionKind::TagRelationApprove,
                    "reject" => ActionKind::TagRelationReject,
                    _ => ActionKind::TagRelationRemove,
                };
                audit(db, user.id, kind, id).await?;
            }
            tracing::info!(id, action, user = user.name, "tag relation updated");
            Ok((
                flash::set(jar, Flash::Saved),
                Redirect::to(path(relation.kind)),
            )
                .into_response())
        }
        Err(RelationError::Rule(rule)) => Err(AppError::BadRequest(rule.to_string())),
        Err(RelationError::Db(e)) => Err(e.into()),
    }
}

/// Records a decision on relation `id` in the audit log.
async fn audit(db: &sqlx::PgPool, actor: i64, kind: ActionKind, id: i32) -> Result<(), AppError> {
    if let Some(relation) = tag_relations::by_id(db, id).await? {
        mod_actions::record(
            db,
            NewAction::new(Some(actor), kind).details(serde_json::json!({
                "relation_id": id,
                "kind": relation.kind.as_str(),
                "antecedent": relation.antecedent,
                "consequent": relation.consequent,
            })),
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uwuu_core::permissions::SystemRole;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    fn form(pairs: &[(&str, &str)]) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish()
    }

    async fn status_of(pool: &PgPool, id: i32) -> Status {
        tag_relations::by_id(pool, id)
            .await
            .unwrap()
            .unwrap()
            .status
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn members_request_and_managers_decide(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, routes());
        let member = session_for(&pool, "alice", SystemRole::Member).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;

        let request = form(&[
            ("antecedent", "Kitty"),
            ("consequent", "cat"),
            ("reason", "same thing"),
        ]);
        assert_eq!(
            app.post_form("/tags/aliases", None, &[], &request)
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        let response = app
            .post_form("/tags/aliases", Some(&member), &[], &request)
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let listed = tag_relations::list(&pool, Kind::Alias, None, "", 0, 10)
            .await
            .unwrap();
        let id = listed[0].id;
        assert_eq!(
            (listed[0].antecedent.as_str(), listed[0].status),
            ("kitty", Status::Pending)
        );

        let page = app.get("/tags/aliases", Some(&member)).await;
        assert!(page.body.contains("same thing"), "{}", page.body);
        assert!(!page.body.contains("/approve"), "members can't approve");
        assert!(page.body.contains(&format!("/tags/aliases/{id}/remove")));
        let approve = format!("/tags/aliases/{id}/approve");
        assert_eq!(
            app.post(&approve, Some(&member), &[]).await.status,
            StatusCode::FORBIDDEN
        );

        let page = app.get("/tags/aliases", Some(&admin)).await;
        assert!(page.body.contains(&approve));
        let response = app.post(&approve, Some(&admin), &[]).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        assert_eq!(status_of(&pool, id).await, Status::Active);
        // Approving twice is refused with an explanation.
        let again = app.post(&approve, Some(&admin), &[]).await;
        assert_eq!(again.status, StatusCode::BAD_REQUEST);

        // Rule violations come back on the form.
        let chain = form(&[("antecedent", "kitten"), ("consequent", "kitty")]);
        let response = app
            .post_form("/tags/aliases", Some(&member), &[], &chain)
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            response
                .body
                .contains("`kitty` is aliased to `cat`; use `cat` instead."),
            "{}",
            response.body
        );
        assert!(response.body.contains("value=\"kitten\""));
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn managers_requests_apply_immediately(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, routes());
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let request = form(&[("antecedent", "tabby"), ("consequent", "cat")]);
        let response = app
            .post_form("/tags/implications", Some(&admin), &[], &request)
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let listed = tag_relations::list(&pool, Kind::Implication, None, "", 0, 10)
            .await
            .unwrap();
        assert_eq!(listed[0].status, Status::Active);

        let bad = form(&[("antecedent", "same"), ("consequent", "SAME")]);
        let response = app
            .post_form("/tags/implications", Some(&admin), &[], &bad)
            .await;
        assert!(response.body.contains("Pick two different tags."));
        let bad = form(&[("antecedent", "-x"), ("consequent", "y")]);
        let response = app
            .post_form("/tags/implications", Some(&admin), &[], &bad)
            .await;
        assert!(
            response
                .body
                .contains("The first tag may not start with `-`."),
            "{}",
            response.body
        );
    }
}
