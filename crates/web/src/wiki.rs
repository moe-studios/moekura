//! Wiki pages: one per tag, with history.

use axum::Form;
use axum::Router;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_core::search::{Query as SearchQuery, TagTerm};
use moekura_core::tags::TagName;
use moekura_db::wiki::{self, SaveError, WikiPage};
use moekura_db::{tag_relations, tags};
use serde::Deserialize;
use sqlx::PgPool;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::tags::{MAX_PAGE, PAGE_SIZE, category_name};
use crate::templates::{search_url, url_value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/wiki", get(index))
        .route("/wiki/{title}", get(show))
        .route("/wiki/{title}/edit", get(edit_form).post(edit))
        .route("/wiki/{title}/history", get(history))
        .route("/wiki/{title}/revert/{version}", post(revert))
}

/// A title from a URL or form, normalised like a tag name.
pub(crate) fn title(raw: &str) -> Result<TagName, AppError> {
    TagName::parse(raw).map_err(|e| AppError::Unprocessable(format!("The title {e}.")))
}

/// How a title reads: `long_hair` as "long hair".
fn display_title(title: &str) -> String {
    title.replace('_', " ")
}

fn page_url(title: &str) -> Value {
    Value::from_safe_string(markup::wiki_url(title))
}

/// Tidies text from a form or the API: browsers send `\r\n`, and trailing
/// whitespace isn't meaningful.
pub(crate) fn clean_body(body: &str) -> Result<String, AppError> {
    let body = body.replace("\r\n", "\n").trim_end().to_owned();
    if body.trim().is_empty() {
        return Err(AppError::Unprocessable("The page is empty.".into()));
    }
    if body.len() > markup::MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "The page is too long: the limit is {} characters.",
            markup::MAX_LEN
        )));
    }
    Ok(body)
}

/// Saves a page as `current`; see [`wiki::save`] for `base`.
pub(crate) async fn save(
    state: &AppState,
    current: &CurrentUser,
    title: &TagName,
    body: &str,
    base: Option<i32>,
) -> Result<i32, SaveError> {
    let updater = current.user.as_ref().map(|u| u.id);
    let version = wiki::save(state.db.primary(), title.as_str(), body, updater, base).await?;
    tracing::info!(title = title.as_str(), version, "wiki page saved");
    Ok(version)
}

pub(crate) fn conflict_message() -> String {
    "Someone else changed this page while you were editing it. Check their changes, \
     then save again."
        .to_owned()
}

/// The first paragraph of the wiki page for a search of a single tag
/// (through its alias, if it has one), for above the results.
pub(crate) async fn search_excerpt(
    db: &PgPool,
    query: &SearchQuery,
) -> Result<Option<Value>, AppError> {
    let [TagTerm::Name(name)] = query.all.as_slice() else {
        return Ok(None);
    };
    if !(query.any.is_empty() && query.none.is_empty() && query.conditions.is_empty()) {
        return Ok(None);
    }
    let title = tag_relations::aliases_of(db, &[name.as_str()])
        .await?
        .pop()
        .map_or_else(|| name.as_str().to_owned(), |(_, consequent)| consequent);
    let Some(page) = wiki::by_title(db, &title).await? else {
        return Ok(None);
    };
    let excerpt = markup::excerpt(&page.body);
    Ok((!excerpt.is_empty()).then(|| {
        context! {
            title => display_title(&page.title),
            url => page_url(&page.title),
            html => Value::from_safe_string(markup::render(&excerpt)),
        }
    }))
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    title: String,
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let number = query.page.unwrap_or(1).clamp(1, MAX_PAGE);
    let pattern = moekura_core::tags::normalize(&query.title);
    let mut found = wiki::list(db, &pattern, (number - 1) * PAGE_SIZE, PAGE_SIZE + 1).await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);
    let rows: Vec<Value> = found
        .iter()
        .map(|p| {
            context! {
                title => display_title(&p.title),
                url => page_url(&p.title),
                version => p.version,
                updater => p.updater_name,
                date => p.updated_at.date().to_string(),
            }
        })
        .collect();
    let list_url = |n: i64| {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("title", &query.title)
            .append_pair("page", &n.to_string())
            .finish();
        url_value(&format!("/wiki?{query}"))
    };
    // Offer to start the page that was looked for.
    let create_url = (page.current.can(Permission::EditWiki) && !pattern.contains('*'))
        .then(|| TagName::parse(&pattern).ok())
        .flatten()
        .filter(|name| !found.iter().any(|p| p.title == name.as_str()))
        .map(|name| Value::from_safe_string(format!("{}/edit", markup::wiki_url(name.as_str()))));
    Ok(page.render(
        "wiki_index.html",
        context! {
            pages => rows,
            query => context! { title => query.title },
            create_url => create_url,
            create_title => display_title(&pattern),
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => has_next.then(|| list_url(number + 1)),
        },
    ))
}

/// Looks up a page for a handler, redirecting to the canonical title
/// when the URL spells it differently (`Long Hair` for `long_hair`).
async fn find(
    db: &PgPool,
    raw: &str,
    suffix: &str,
) -> Result<Result<(TagName, Option<WikiPage>), Response>, AppError> {
    let title = TagName::parse(raw).map_err(|_| AppError::NotFound)?;
    if title.as_str() != raw {
        let url = format!("{}{suffix}", markup::wiki_url(title.as_str()));
        return Ok(Err(Redirect::permanent(&url).into_response()));
    }
    let page = wiki::by_title(db, title.as_str()).await?;
    Ok(Ok((title, page)))
}

#[derive(Debug, Default, Deserialize)]
struct ShowQuery {
    version: Option<i32>,
}

async fn show(
    page: Page,
    Path(raw): Path<String>,
    Query(query): Query<ShowQuery>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let (title, wiki_page) = match find(db, &raw, "").await? {
        Ok(found) => found,
        Err(redirect) => return Ok(redirect),
    };
    let tag = tags::by_name(db, title.as_str()).await?;
    let categories = tags::categories(db).await?;
    let alias_of = tag_relations::aliases_of(db, &[title.as_str()])
        .await?
        .pop()
        .map(|(_, consequent)| context! { title => display_title(&consequent), url => page_url(&consequent) });
    let (body, old_version) = match (&wiki_page, query.version) {
        (Some(p), Some(v)) if v != p.version => {
            let old = wiki::version(db, p.id, v)
                .await?
                .ok_or(AppError::NotFound)?;
            (Some(old.body), Some(v))
        }
        (Some(p), _) => (Some(p.body.clone()), None),
        (None, _) => (None, None),
    };
    let context = context! {
        title => display_title(title.as_str()),
        name => title.as_str(),
        url => page_url(title.as_str()),
        html => body.map(|b| Value::from_safe_string(markup::render(&b))),
        old_version => old_version,
        wiki => wiki_page.as_ref().map(|p| context! {
            version => p.version,
            updater => p.updater_name,
            date => p.updated_at.date().to_string(),
        }),
        tag => tag.map(|t| context! {
            count => t.post_count,
            category => category_name(&categories, t.category_id),
            deprecated => t.is_deprecated,
        }),
        alias_of => alias_of,
        search_url => Value::from_safe_string(search_url(title.as_str())),
        can_edit => page.current.can(Permission::EditWiki),
        preview => wiki_page.as_ref().and_then(|p| crate::previews::meta(
            page.state(),
            &markup::wiki_url(&p.title),
            &format!("{} · Wiki", display_title(&p.title)),
            &markup::excerpt(&p.body),
            None,
            false,
        )),
    };
    let status = if wiki_page.is_some() {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    Ok(page.render_with_status(status, "wiki_page.html", context))
}

fn edit_context(title: &TagName, body: &str, base: i32, error: Option<String>) -> Value {
    context! {
        title => display_title(title.as_str()),
        url => page_url(title.as_str()),
        body => body,
        base => base,
        is_new => base == 0,
        error => error,
        max_len => markup::MAX_LEN,
    }
}

async fn edit_form(page: Page, Path(raw): Path<String>) -> Result<Response, AppError> {
    page.current.require(Permission::EditWiki)?;
    let (title, wiki_page) = match find(page.state().db.primary(), &raw, "/edit").await? {
        Ok(found) => found,
        Err(redirect) => return Ok(redirect),
    };
    let (body, base) = wiki_page.map_or((String::new(), 0), |p| (p.body, p.version));
    Ok(page.render("wiki_edit.html", edit_context(&title, &body, base, None)))
}

#[derive(Debug, Deserialize)]
struct EditForm {
    body: String,
    /// The version the form was loaded with; 0 for a new page.
    base: i32,
}

async fn edit(
    page: Page,
    jar: CookieJar,
    Path(raw): Path<String>,
    Form(form): Form<EditForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditWiki)?;
    let title = title(&raw)?;
    let body = match clean_body(&form.body) {
        Ok(body) => body,
        Err(error) => {
            return Ok(page.render_with_status(
                error.status(),
                "wiki_edit.html",
                edit_context(
                    &title,
                    &form.body,
                    form.base,
                    Some(error.public_message().to_owned()),
                ),
            ));
        }
    };
    match save(page.state(), &page.current, &title, &body, Some(form.base)).await {
        Ok(_) => Ok((
            flash::set(jar, Flash::Saved),
            Redirect::to(&markup::wiki_url(title.as_str())),
        )
            .into_response()),
        // Keep their text, and let them save over the newer version once
        // they've looked at it.
        Err(SaveError::Conflict { current, .. }) => Ok(page.render_with_status(
            StatusCode::CONFLICT,
            "wiki_edit.html",
            edit_context(&title, &body, current, Some(conflict_message())),
        )),
        Err(SaveError::Db(e)) => Err(e.into()),
    }
}

async fn history(page: Page, Path(raw): Path<String>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let (title, wiki_page) = match find(db, &raw, "/history").await? {
        Ok(found) => found,
        Err(redirect) => return Ok(redirect),
    };
    let wiki_page = wiki_page.ok_or(AppError::NotFound)?;
    let versions = wiki::versions(db, wiki_page.id).await?;
    let can_revert = page.current.can(Permission::EditWiki);
    let base = markup::wiki_url(title.as_str());
    let rows: Vec<Value> = versions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let previous = versions.get(i + 1);
            context! {
                version => v.version,
                date => v.created_at.date().to_string(),
                updater => v.updater_name,
                url => url_value(&format!("{base}?version={}", v.version)),
                // Characters, as a rough measure of the change.
                change => previous.map(|p| v.body.chars().count() as i64 - p.body.chars().count() as i64),
                current => i == 0,
                can_revert => can_revert && i > 0,
            }
        })
        .collect();
    Ok(page.render(
        "wiki_history.html",
        context! {
            title => display_title(title.as_str()),
            url => page_url(title.as_str()),
            versions => rows,
        },
    ))
}

/// Saves an earlier version's text as a new version.
async fn revert(
    page: Page,
    jar: CookieJar,
    Path((raw, version)): Path<(String, i32)>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditWiki)?;
    let db = page.state().db.primary();
    let title = title(&raw)?;
    let wiki_page = wiki::by_title(db, title.as_str())
        .await?
        .ok_or(AppError::NotFound)?;
    let old = wiki::version(db, wiki_page.id, version)
        .await?
        .ok_or(AppError::NotFound)?;
    save(page.state(), &page.current, &title, &old.body, None)
        .await
        .map_err(|e| match e {
            SaveError::Db(e) => AppError::from(e),
            SaveError::Conflict { .. } => AppError::Conflict(conflict_message()),
        })?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("{}/history", markup::wiki_url(title.as_str()))),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    fn app_routes() -> Router<AppState> {
        routes().merge(crate::posts::routes())
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn create_edit_and_view(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, app_routes());
        let missing = app.get("/wiki/long_hair", None).await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert!(!missing.body.contains("/wiki/long_hair/edit"));

        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let missing = app.get("/wiki/long_hair", Some(&alice)).await;
        assert!(
            missing.body.contains("/wiki/long_hair/edit"),
            "{}",
            missing.body
        );
        // Other spellings lead to the canonical page.
        let other = app.get("/wiki/Long%20Hair/edit", Some(&alice)).await;
        assert_eq!(other.status, StatusCode::PERMANENT_REDIRECT);
        assert_eq!(other.location.as_deref(), Some("/wiki/long_hair/edit"));
        assert!(
            app.get("/wiki/long_hair/edit", Some(&alice))
                .await
                .body
                .contains("name=\"base\" value=\"0\"")
        );

        let saved = app
            .post_form(
                "/wiki/long_hair/edit",
                Some(&alice),
                &[],
                "base=0&body=Hair+that+is+long.%0D%0A%0D%0ASee+%5B%5Bshort_hair%5D%5D.",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        assert_eq!(saved.location.as_deref(), Some("/wiki/long_hair"));
        let shown = app.get("/wiki/long_hair", None).await;
        assert_eq!(shown.status, StatusCode::OK);
        assert!(
            shown.body.contains("<p>Hair that is long.</p>"),
            "{}",
            shown.body
        );
        assert!(shown.body.contains("href=\"/wiki/short_hair\""));

        // A second editor who started from version 0 would overwrite it.
        let stale = app
            .post_form(
                "/wiki/long_hair/edit",
                Some(&alice),
                &[],
                "base=0&body=Mine",
            )
            .await;
        assert_eq!(stale.status, StatusCode::CONFLICT);
        assert!(
            stale.body.contains("Someone else changed"),
            "{}",
            stale.body
        );
        assert!(stale.body.contains(">Mine</textarea>"), "keeps their text");
        assert!(stale.body.contains("name=\"base\" value=\"1\""));

        let empty = app
            .post_form("/wiki/long_hair/edit", Some(&alice), &[], "base=1&body=+")
            .await;
        assert_eq!(empty.status, StatusCode::UNPROCESSABLE_ENTITY);

        app.post_form(
            "/wiki/long_hair/edit",
            Some(&alice),
            &[],
            "base=1&body=Changed",
        )
        .await;
        let history = app.get("/wiki/long_hair/history", None).await;
        assert!(history.body.contains("Version 2"), "{}", history.body);
        assert!(!history.body.contains("/revert/"), "visitors can't revert");
        let old = app.get("/wiki/long_hair?version=1", None).await;
        assert!(old.body.contains("Hair that is long."));
        assert!(old.body.contains("an old version"), "{}", old.body);

        let reverted = app
            .post("/wiki/long_hair/revert/1", Some(&alice), &[])
            .await;
        assert_eq!(reverted.status, StatusCode::SEE_OTHER, "{}", reverted.body);
        let page = wiki::by_title(&pool, "long_hair").await.unwrap().unwrap();
        assert_eq!(page.version, 3);
        assert!(page.body.starts_with("Hair that is long."));

        let list = app.get("/wiki?title=long", None).await;
        assert!(
            list.body.contains("href=\"/wiki/long_hair\""),
            "{}",
            list.body
        );

        // Titles may contain `/`, encoded in the URL.
        wiki::save(&pool, "fate/stay_night", "A series.", None, None)
            .await
            .unwrap();
        let slash = app.get("/wiki/fate%2Fstay_night", None).await;
        assert_eq!(slash.status, StatusCode::OK);
        assert!(slash.body.contains("A series."));
        assert!(
            slash
                .body
                .contains("href=\"/wiki/fate%2Fstay_night/history\"")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn editing_needs_permission(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, app_routes());
        assert_eq!(
            app.get("/wiki/cat/edit", None).await.location.as_deref(),
            Some("/login?next=%2Fwiki%2Fcat%2Fedit")
        );
        let post = app
            .post_form("/wiki/cat/edit", None, &[], "base=0&body=x")
            .await;
        assert_eq!(post.status, StatusCode::UNAUTHORIZED);
        assert!(wiki::by_title(&pool, "cat").await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn excerpt_above_single_tag_searches(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, app_routes());
        wiki::save(
            &pool,
            "cat",
            "h1. Cats\n\nA small [[animal]].\n\nMore.",
            None,
            None,
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('alias', 'kitty', 'cat', 'active')",
        )
        .execute(&pool)
        .await
        .unwrap();
        for search in ["cat", "kitty"] {
            let page = app.get(&format!("/posts?tags={search}"), None).await;
            assert!(
                page.body
                    .contains("A small <a class=\"wiki-link\" href=\"/wiki/animal\">animal</a>."),
                "{search}: {}",
                page.body
            );
            assert!(!page.body.contains("More."));
        }
        for search in ["cat+dog", "-cat", "cat+rating:g"] {
            let page = app.get(&format!("/posts?tags={search}"), None).await;
            assert!(!page.body.contains("A small"), "{search}");
        }
    }
}
