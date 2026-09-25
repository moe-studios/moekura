//! Saved searches: a user's own list, and `search:<label>` to see their
//! posts together.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::search::{Filter, Query as SearchQuery};
use moekura_db::saved_searches::{self, SavedSearch, UpdateError};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::{search_url, url_value};

/// Most searches one user may save.
pub const MAX_SAVED: i64 = 100;

/// Most labels on one saved search.
const MAX_LABELS: usize = 10;

const LABEL_MAX_LEN: usize = 64;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/saved_searches", get(index).post(create))
        .route("/saved_searches/{id}", post(update))
        .route("/saved_searches/{id}/delete", post(delete))
}

fn user_id(current: &CurrentUser) -> Result<i64, AppError> {
    current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)
}

/// A search to save, normalised.
pub(crate) fn clean_query(text: &str) -> Result<String, AppError> {
    let query = SearchQuery::parse(text).map_err(|e| AppError::Unprocessable(format!("{e}.")))?;
    if query.is_empty() {
        return Err(AppError::Unprocessable("The search is empty.".into()));
    }
    if query
        .conditions
        .iter()
        .any(|c| matches!(c.filter, Filter::Search(_)))
    {
        return Err(AppError::Unprocessable(
            "A saved search can't use search: itself.".into(),
        ));
    }
    let normalized = query.to_string();
    if normalized.len() > 1000 {
        return Err(AppError::Unprocessable("The search is too long.".into()));
    }
    Ok(normalized)
}

/// Labels typed as words, lower case, without duplicates.
pub(crate) fn clean_labels(text: &str) -> Result<Vec<String>, AppError> {
    let mut labels: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == ',') {
        if word.is_empty() {
            continue;
        }
        let label = word.to_lowercase();
        let valid = label.chars().count() <= LABEL_MAX_LEN
            && label
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
            && label != "all";
        if !valid {
            return Err(AppError::Unprocessable(format!(
                "“{word}” can't be a label: use letters, digits, _ and -, and not “all”."
            )));
        }
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    if labels.len() > MAX_LABELS {
        return Err(AppError::Unprocessable(format!(
            "A saved search can have at most {MAX_LABELS} labels."
        )));
    }
    Ok(labels)
}

/// Saves `query` with `labels` for `current`, or relabels it if it's
/// already saved.
pub(crate) async fn save(
    state: &AppState,
    current: &CurrentUser,
    query: &str,
    labels: &str,
) -> Result<i64, AppError> {
    let user = user_id(current)?;
    let query = clean_query(query)?;
    let labels = clean_labels(labels)?;
    let db = state.db.primary();
    let existing = saved_searches::for_user(db, user).await?;
    if !existing.iter().any(|s| s.query == query) && existing.len() as i64 >= MAX_SAVED {
        return Err(AppError::Unprocessable(format!(
            "You can save at most {MAX_SAVED} searches."
        )));
    }
    Ok(saved_searches::save(db, user, &query, &labels).await?)
}

pub(crate) fn update_error(error: UpdateError) -> AppError {
    match error {
        UpdateError::Duplicate => AppError::Unprocessable(error.to_string()),
        UpdateError::Db(e) => e.into(),
    }
}

fn row_context(search: &SavedSearch) -> Value {
    context! {
        id => search.id,
        query => search.query,
        url => Value::from_safe_string(search_url(&search.query)),
        labels => search.labels.join(" "),
        label_links => search.labels.iter().map(|l| context! {
            label => l,
            url => Value::from_safe_string(search_url(&format!("search:{l}"))),
        }).collect::<Vec<_>>(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    /// A search to fill the form with.
    #[serde(default)]
    query: String,
}

async fn render(
    page: &Page,
    query: &str,
    labels: &str,
    error: Option<String>,
) -> Result<Response, AppError> {
    let user = user_id(&page.current)?;
    let saved = saved_searches::for_user(page.state().db.primary(), user).await?;
    let mut all_labels: Vec<&str> = saved
        .iter()
        .flat_map(|s| s.labels.iter().map(String::as_str))
        .collect();
    all_labels.sort_unstable();
    all_labels.dedup();
    let status = error.as_ref().map_or(axum::http::StatusCode::OK, |_| {
        axum::http::StatusCode::UNPROCESSABLE_ENTITY
    });
    Ok(page.render_with_status(
        status,
        "saved_searches.html",
        context! {
            searches => saved.iter().map(row_context).collect::<Vec<_>>(),
            labels => all_labels.iter().map(|l| context! {
                label => l,
                url => Value::from_safe_string(search_url(&format!("search:{l}"))),
            }).collect::<Vec<_>>(),
            all_url => Value::from_safe_string(search_url("search:all")),
            form => context! { query => query, labels => labels },
            error => error,
            max => MAX_SAVED,
        },
    ))
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    render(&page, &query.query, "", None).await
}

#[derive(Debug, Deserialize)]
struct SaveForm {
    query: String,
    #[serde(default)]
    labels: String,
    /// Go back to the search's results after saving.
    #[serde(default)]
    back: bool,
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<SaveForm>,
) -> Result<Response, AppError> {
    match save(page.state(), &page.current, &form.query, &form.labels).await {
        Ok(_) => {
            let to = if form.back {
                search_url(&clean_query(&form.query).unwrap_or_default())
            } else {
                "/saved_searches".to_owned()
            };
            Ok((flash::set(jar, Flash::Saved), Redirect::to(&to)).into_response())
        }
        Err(AppError::Unprocessable(message)) => {
            render(&page, &form.query, &form.labels, Some(message)).await
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug, Deserialize)]
struct UpdateForm {
    query: String,
    #[serde(default)]
    labels: String,
}

async fn update(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<UpdateForm>,
) -> Result<Response, AppError> {
    let user = user_id(&page.current)?;
    let checked = clean_query(&form.query).and_then(|q| Ok((q, clean_labels(&form.labels)?)));
    let (query, labels) = match checked {
        Ok(checked) => checked,
        Err(AppError::Unprocessable(message)) => {
            return render(&page, "", "", Some(message)).await;
        }
        Err(error) => return Err(error),
    };
    match saved_searches::update(page.state().db.primary(), user, id, &query, &labels).await {
        Ok(true) => {}
        Ok(false) => return Err(AppError::NotFound),
        Err(error) => match update_error(error) {
            AppError::Unprocessable(message) => return render(&page, "", "", Some(message)).await,
            error => return Err(error),
        },
    }
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/saved_searches"),
    )
        .into_response())
}

async fn delete(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    let user = user_id(&page.current)?;
    if !saved_searches::remove(page.state().db.primary(), user, id).await? {
        return Err(AppError::NotFound);
    }
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/saved_searches"),
    )
        .into_response())
}

/// The "save this search" form on search results, for `query`.
pub(crate) fn save_form(current: &CurrentUser, query: &str) -> Option<Value> {
    (current.is_logged_in() && !query.trim().is_empty()).then(|| {
        context! {
            query => query,
            list_url => url_value("/saved_searches"),
        }
    })
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    #[test]
    fn labels_and_queries() {
        assert_eq!(clean_labels(" Pets, cats pets ").unwrap(), ["pets", "cats"]);
        assert!(clean_labels("a/b").is_err());
        assert!(clean_labels("ALL").is_err());
        assert_eq!(clean_query("  cat   rating:G ").unwrap(), "cat rating:g");
        assert!(clean_query("").is_err());
        assert!(clean_query("cat search:all").is_err());
        assert!(clean_query("-").is_err());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn save_list_edit_and_remove(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            routes().merge(crate::posts::routes()),
        );
        assert_eq!(
            app.get("/saved_searches", None).await.location.as_deref(),
            Some("/login?next=%2Fsaved_searches")
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;

        // From the results page, back to the results.
        let results = app.get("/posts?tags=cat", Some(&alice)).await;
        assert!(
            results.body.contains("action=\"/saved_searches\""),
            "{}",
            results.body
        );
        let saved = app
            .post_form(
                "/saved_searches",
                Some(&alice),
                &[],
                "query=cat&labels=Pets&back=true",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        assert_eq!(saved.location.as_deref(), Some("/posts?tags=cat"));

        let bad = app
            .post_form(
                "/saved_searches",
                Some(&alice),
                &[],
                "query=dog&labels=a%2Fb",
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(bad.body.contains("can&#x27;t be a label"), "{}", bad.body);
        assert!(bad.body.contains("value=\"dog\""), "keeps the form");

        let list = app.get("/saved_searches", Some(&alice)).await;
        assert!(
            list.body.contains("href=\"/posts?tags=search%3Apets\""),
            "{}",
            list.body
        );
        let id: i64 = sqlx::query_scalar("SELECT id FROM saved_searches")
            .fetch_one(&pool)
            .await
            .unwrap();
        // Only the owner changes it.
        assert_eq!(
            app.post_form(&format!("/saved_searches/{id}"), Some(&bob), &[], "query=x")
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        app.post_form(
            &format!("/saved_searches/{id}"),
            Some(&alice),
            &[],
            "query=cat+ears&labels=",
        )
        .await;
        let query: String = sqlx::query_scalar("SELECT query FROM saved_searches")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(query, "cat ears");
        assert_eq!(
            app.post(&format!("/saved_searches/{id}/delete"), Some(&bob), &[])
                .await
                .status,
            StatusCode::NOT_FOUND
        );
        app.post(&format!("/saved_searches/{id}/delete"), Some(&alice), &[])
            .await;
        assert!(
            app.get("/saved_searches", Some(&alice))
                .await
                .body
                .contains("saved any searches yet")
        );
    }
}
