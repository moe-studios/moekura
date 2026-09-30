//! Artist commentary on posts: the title and description the artist gave
//! a work where they posted it, and translations, with history.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_db::artist_commentaries::{self, Commentary, SaveError, Texts, VersionFilter};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::url_value;

/// The longest title, in characters.
pub(crate) const TITLE_MAX_LEN: usize = 1000;
/// The longest description, in characters.
pub(crate) const DESCRIPTION_MAX_LEN: usize = 50_000;

/// Versions per page of history.
const VERSIONS_PAGE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/posts/{id}/commentary", post(save))
        .route("/posts/{id}/commentary/history", get(history))
        .route("/artist_commentary_versions", get(recent))
}

/// Tidies and checks commentary texts from a form or an API.
pub(crate) fn clean(texts: Texts) -> Result<Texts, AppError> {
    let tidy = |text: String| text.replace("\r\n", "\n").trim().to_owned();
    let texts = Texts {
        original_title: tidy(texts.original_title),
        original_description: tidy(texts.original_description),
        translated_title: tidy(texts.translated_title),
        translated_description: tidy(texts.translated_description),
    };
    if [&texts.original_title, &texts.translated_title]
        .iter()
        .any(|t| t.chars().count() > TITLE_MAX_LEN)
    {
        return Err(AppError::Unprocessable(format!(
            "Titles can be at most {TITLE_MAX_LEN} characters long."
        )));
    }
    if [&texts.original_description, &texts.translated_description]
        .iter()
        .any(|t| t.chars().count() > DESCRIPTION_MAX_LEN)
    {
        return Err(AppError::Unprocessable(format!(
            "Descriptions can be at most {DESCRIPTION_MAX_LEN} characters long."
        )));
    }
    Ok(texts)
}

/// Saves post `post_id`'s commentary as `current`, who must be able to
/// see and edit the post; see [`artist_commentaries::save`] for `base`.
pub(crate) async fn save_commentary(
    state: &AppState,
    current: &CurrentUser,
    post_id: i64,
    texts: Texts,
    base: Option<i32>,
) -> Result<i32, AppError> {
    current.require(Permission::EditPosts)?;
    let db = state.db.primary();
    let post = moekura_db::posts::by_id(db, post_id)
        .await?
        .filter(|p| crate::posts::visibility(current).allows(p))
        .ok_or(AppError::NotFound)?;
    let texts = clean(texts)?;
    artist_commentaries::save(
        db,
        post.id,
        &texts,
        current.user.as_ref().map(|u| u.id),
        base,
    )
    .await
    .map_err(|e| match e {
        SaveError::Conflict { .. } => AppError::Conflict(
            "Someone else changed the commentary while you were editing it. Check their \
             changes, then save again."
                .into(),
        ),
        SaveError::Db(e) => e.into(),
    })
}

fn texts_context(texts: &Texts) -> Value {
    let html =
        |text: &str| (!text.is_empty()).then(|| Value::from_safe_string(markup::render(text)));
    context! {
        original_title => texts.original_title,
        original_html => html(&texts.original_description),
        translated_title => texts.translated_title,
        translated_html => html(&texts.translated_description),
        original => !(texts.original_title.is_empty() && texts.original_description.is_empty()),
        translated => texts.is_translated(),
    }
}

/// The commentary panel of a post page: what's shown, and the form when
/// `can_edit`.
pub(crate) async fn for_post(
    db: &sqlx::PgPool,
    post_id: i64,
    can_edit: bool,
) -> Result<Option<Value>, AppError> {
    let found = artist_commentaries::for_post(db, post_id).await?;
    if found.is_none() && !can_edit {
        return Ok(None);
    }
    let texts = found
        .as_ref()
        .map_or_else(Texts::default, |c: &Commentary| c.texts.clone());
    Ok(Some(context! {
        shown => found.as_ref().map(|c| texts_context(&c.texts)),
        form => can_edit.then(|| context! {
            original_title => texts.original_title,
            original_description => texts.original_description,
            translated_title => texts.translated_title,
            translated_description => texts.translated_description,
            base => found.as_ref().map_or(0, |c| c.version),
        }),
        history_url => found.is_some().then(|| url_value(&format!("/posts/{post_id}/commentary/history"))),
    }))
}

#[derive(Debug, Deserialize)]
struct CommentaryForm {
    #[serde(default)]
    original_title: String,
    #[serde(default)]
    original_description: String,
    #[serde(default)]
    translated_title: String,
    #[serde(default)]
    translated_description: String,
    /// The version the form was loaded with; 0 for none.
    #[serde(default)]
    base: i32,
}

async fn save(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<CommentaryForm>,
) -> Result<Response, AppError> {
    let texts = Texts {
        original_title: form.original_title,
        original_description: form.original_description,
        translated_title: form.translated_title,
        translated_description: form.translated_description,
    };
    save_commentary(page.state(), &page.current, id, texts, Some(form.base)).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/posts/{id}#commentary")),
    )
        .into_response())
}

#[derive(Debug, Default, Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    user: String,
    before: Option<i64>,
}

async fn render_versions(
    page: &Page,
    post_id: Option<i64>,
    query: &HistoryQuery,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let updater_id = match query.user.trim() {
        "" => None,
        name => Some(
            moekura_db::users::by_name(db, name)
                .await?
                .map_or(-1, |u| u.id),
        ),
    };
    let filter = VersionFilter {
        post_id,
        updater_id,
        before: query.before,
        offset: 0,
    };
    let found = artist_commentaries::versions(db, filter, VERSIONS_PAGE + 1).await?;
    let more = found.len() > VERSIONS_PAGE as usize;
    // Only for posts the viewer may see.
    let ids: Vec<i64> = found.iter().map(|v| v.post_id).collect();
    let visible = crate::posts::visibility(&page.current);
    let posts = moekura_db::posts::by_ids(db, &ids).await?;
    let path = post_id.map_or_else(
        || "/artist_commentary_versions".to_owned(),
        |id| format!("/posts/{id}/commentary/history"),
    );
    let versions: Vec<Value> = found
        .iter()
        .take(VERSIONS_PAGE as usize)
        .filter(|v| posts.iter().any(|p| p.id == v.post_id && visible.allows(p)))
        .map(|v| {
            context! {
                post_id => v.post_id,
                version => v.version,
                date => crate::dates::day(v.created_at),
                time => crate::dates::clock(v.created_at),
                updater => v.updater_name,
                texts => texts_context(&v.texts),
            }
        })
        .collect();
    let next_url = more
        .then(|| found.get(VERSIONS_PAGE as usize - 1))
        .flatten()
        .map(|last| {
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("user", &query.user)
                .append_pair("before", &last.id.to_string())
                .finish();
            url_value(&format!("{path}?{q}"))
        });
    Ok(page.render(
        "commentary_versions.html",
        context! {
            post_id => post_id,
            path => path,
            query => context! { user => query.user },
            versions => versions,
            next_url => next_url,
        },
    ))
}

async fn history(
    page: Page,
    Path(id): Path<i64>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, AppError> {
    let db = page.state().reader(&page.current);
    moekura_db::posts::by_id(db, id)
        .await?
        .filter(|p| crate::posts::visibility(&page.current).allows(p))
        .ok_or(AppError::NotFound)?;
    render_versions(&page, Some(id), &query).await
}

async fn recent(page: Page, Query(query): Query<HistoryQuery>) -> Result<Response, AppError> {
    render_versions(&page, None, &query).await
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn edit_show_search_and_history(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let post = crate::danbooru::test_support::upload(&app, &alice, 20, "cat").await;
        let page = app.get(&format!("/posts/{post}"), Some(&alice)).await.body;
        assert!(page.contains("Artist's commentary"), "{page}");
        assert!(
            !app.get(&format!("/posts/{post}"), None)
                .await
                .body
                .contains("Artist's commentary")
        );

        let url = format!("/posts/{post}/commentary");
        assert_eq!(
            app.post_form(&url, None, &[], "original_title=x&base=0")
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        let saved = app
            .post_form(
                &url,
                Some(&alice),
                &[],
                "original_title=%E7%8C%AB&original_description=%E6%96%B0%E4%BD%9C&base=0",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        let stale = app
            .post_form(&url, Some(&alice), &[], "original_title=y&base=0")
            .await;
        assert_eq!(stale.status, StatusCode::CONFLICT);
        let untranslated = app
            .get("/posts?tags=commentary%3Auntranslated", None)
            .await
            .body;
        assert!(
            untranslated.contains(&format!("href=\"/posts/{post}")),
            "{untranslated}"
        );
        app.post_form(
            &url,
            Some(&alice),
            &[],
            "original_title=%E7%8C%AB&original_description=%E6%96%B0%E4%BD%9C\
             &translated_title=Cat&translated_description=A+new+work&base=1",
        )
        .await;
        let shown = app.get(&format!("/posts/{post}"), None).await.body;
        assert!(
            shown.contains("A new work") && shown.contains("新作"),
            "{shown}"
        );
        let found = app
            .get("/posts?tags=commentary%3Anew_work", None)
            .await
            .body;
        assert!(found.contains(&format!("href=\"/posts/{post}")), "{found}");
        let none = app.get("/posts?tags=commentary%3Afalse", None).await.body;
        assert!(!none.contains(&format!("href=\"/posts/{post}")), "{none}");

        let history = app.get(&format!("{url}/history"), None).await.body;
        assert!(
            history.contains("version 2") && history.contains("version 1"),
            "{history}"
        );
        let recent = app
            .get("/artist_commentary_versions?user=alice", None)
            .await
            .body;
        assert!(recent.contains(&format!("/posts/{post}")), "{recent}");
    }
}
