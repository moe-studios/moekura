//! Mass tag edits: adding and removing tags on every post matching a
//! search, in the background, with a preview first.

use axum::Router;
use axum::extract::Form;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::jobs::MassUpdate;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_core::search::Query as SearchQuery;
use moekura_core::tags::TagName;
use moekura_db::mass_updates;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::search::{PageRef, Plan, SearchError};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::visibility;

/// Recent mass edits listed.
const RECENT: i64 = 20;

/// Posts shown in a preview.
const PREVIEW: u32 = 20;

pub fn routes() -> Router<AppState> {
    Router::new().route("/moderation/mass-edit", get(page).post(submit))
}

/// Tag names typed as words, checked.
pub(crate) fn tag_list(text: &str) -> Result<Vec<String>, AppError> {
    let mut names: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let name = TagName::parse(word)
            .map_err(|e| AppError::Unprocessable(format!("“{word}”: the tag {e}.")))?
            .into_string();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

/// A mass edit, checked: the search normalised, and the tags to add and
/// remove.
pub(crate) struct Checked {
    pub query: String,
    pub add: Vec<String>,
    pub remove: Vec<String>,
}

pub(crate) fn check(query: &str, add: &str, remove: &str) -> Result<Checked, AppError> {
    let parsed = SearchQuery::parse(query).map_err(|e| AppError::Unprocessable(format!("{e}.")))?;
    if parsed.is_empty() {
        return Err(AppError::Unprocessable(
            "Give a search: a mass edit of every post isn't allowed.".into(),
        ));
    }
    let add = tag_list(add)?;
    let remove = tag_list(remove)?;
    if add.is_empty() && remove.is_empty() {
        return Err(AppError::Unprocessable(
            "Give tags to add or remove.".into(),
        ));
    }
    if let Some(both) = add.iter().find(|t| remove.contains(t)) {
        return Err(AppError::Unprocessable(format!(
            "“{both}” is both added and removed."
        )));
    }
    Ok(Checked {
        query: parsed.to_string(),
        add,
        remove,
    })
}

/// Starts a checked mass edit as `current`; returns its id.
pub(crate) async fn start(
    state: &AppState,
    current: &CurrentUser,
    edit: &Checked,
) -> Result<i64, AppError> {
    current.require(Permission::MassEditTags)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = state.db.primary().begin().await?;
    let id = mass_updates::create(&mut *tx, actor, &edit.query, &edit.add, &edit.remove).await?;
    moekura_db::jobs::enqueue(&mut tx, &MassUpdate { id }).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::MassUpdate).details(serde_json::json!({
            "id": id,
            "query": edit.query,
            "add": edit.add,
            "remove": edit.remove,
        })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(id, query = edit.query, "mass tag edit started");
    Ok(id)
}

fn update_context(u: &mass_updates::MassUpdate) -> Value {
    context! {
        id => u.id,
        by => u.creator_name,
        query => u.query,
        search_url => Value::from_safe_string(crate::templates::search_url(&u.query)),
        add => u.add_tags,
        remove => u.remove_tags,
        status => u.status,
        seen => u.seen,
        changed => u.changed,
        error => u.error,
        when => u.created_at.date().to_string(),
    }
}

async fn render(
    page: &Page,
    form: &MassEditForm,
    preview: Option<Value>,
    error: Option<String>,
) -> Result<Response, AppError> {
    let recent = mass_updates::recent(page.state().db.primary(), RECENT).await?;
    let status = if error.is_some() {
        axum::http::StatusCode::UNPROCESSABLE_ENTITY
    } else {
        axum::http::StatusCode::OK
    };
    Ok(page.render_with_status(
        status,
        "mass_edit.html",
        context! {
            form => context! { query => form.query, add => form.add, remove => form.remove },
            preview => preview,
            error => error,
            recent => recent.iter().map(update_context).collect::<Vec<_>>(),
        },
    ))
}

async fn page(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::MassEditTags)?;
    render(&page, &MassEditForm::default(), None, None).await
}

#[derive(Debug, Default, Deserialize)]
struct MassEditForm {
    #[serde(default)]
    query: String,
    #[serde(default)]
    add: String,
    #[serde(default)]
    remove: String,
    /// `preview` or `run`.
    #[serde(default)]
    step: String,
}

async fn submit(
    page: Page,
    jar: CookieJar,
    Form(form): Form<MassEditForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::MassEditTags)?;
    let checked = match check(&form.query, &form.add, &form.remove) {
        Ok(checked) => checked,
        Err(AppError::Unprocessable(message)) => {
            return render(&page, &form, None, Some(message)).await;
        }
        Err(error) => return Err(error),
    };
    if form.step == "run" {
        start(page.state(), &page.current, &checked).await?;
        return Ok((
            flash::set(jar, Flash::Saved),
            Redirect::to("/moderation/mass-edit"),
        )
            .into_response());
    }
    // A preview: how many posts, and the first of them.
    let state = page.state();
    let db = state.db.primary();
    let query =
        SearchQuery::parse(&checked.query).map_err(|e| AppError::Unprocessable(e.to_string()))?;
    let config = moekura_core::config::SearchConfig {
        per_page: PREVIEW,
        ..state.config.search.clone()
    };
    let plan = match Plan::resolve(db, &query, &visibility(&page.current), &config).await {
        Ok(plan) => plan,
        Err(SearchError::Invalid(message)) => {
            return render(&page, &form, None, Some(message)).await;
        }
        Err(SearchError::Db(e)) => return Err(e.into()),
    };
    let count = plan.count(db).await.map_err(|e| match e {
        SearchError::Invalid(message) => AppError::Unprocessable(message),
        SearchError::Db(e) => e.into(),
    })?;
    let ids = plan
        .ids(db, PageRef::Number(1))
        .await
        .map_err(|e| match e {
            SearchError::Invalid(message) => AppError::Unprocessable(message),
            SearchError::Db(e) => e.into(),
        })?;
    let cards: Vec<Value> = crate::posts::grid(&page, db, &ids, None)
        .await?
        .into_iter()
        .map(|(_, card)| card)
        .collect();
    let preview = context! {
        count => crate::posts::count_text(count),
        cards => cards,
        query => checked.query,
        add => checked.add,
        remove => checked.remove,
    };
    let form = MassEditForm {
        query: checked.query.clone(),
        add: checked.add.join(" "),
        remove: checked.remove.join(" "),
        step: String::new(),
    };
    render(&page, &form, Some(preview), None).await
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    #[test]
    fn checks_edits() {
        assert!(check("", "a", "").is_err());
        assert!(check("cat", "", "").is_err());
        assert!(check("cat", "a", "A").is_err());
        let ok = check(" Cat  -dog ", "Animal ears", "cat").unwrap();
        assert_eq!(
            (ok.query.as_str(), ok.add, ok.remove),
            (
                "cat -dog",
                vec!["animal".to_owned(), "ears".to_owned()],
                vec!["cat".to_owned()]
            )
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn preview_then_run(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, routes());
        let janitor = session_for(&pool, "jan", SystemRole::Janitor).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        assert_eq!(
            app.get("/moderation/mass-edit", Some(&janitor))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let tag: i32 = sqlx::query_scalar("INSERT INTO tags (name) VALUES ('cat') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        for _ in 0..3 {
            sqlx::query("INSERT INTO posts (rating, tag_ids) VALUES ('g', ARRAY[$1])")
                .bind(tag)
                .execute(&pool)
                .await
                .unwrap();
        }

        let preview = app
            .post_form(
                "/moderation/mass-edit",
                Some(&moderator),
                &[],
                "query=cat&add=animal&remove=cat&step=preview",
            )
            .await;
        assert_eq!(preview.status, StatusCode::OK, "{}", preview.body);
        assert!(preview.body.contains("Change 3 posts"), "{}", preview.body);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mass_updates")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0,
            "a preview changes nothing"
        );
        let bad = app
            .post_form(
                "/moderation/mass-edit",
                Some(&moderator),
                &[],
                "query=&add=x",
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

        let run = app
            .post_form(
                "/moderation/mass-edit",
                Some(&moderator),
                &[],
                "query=cat&add=animal&remove=cat&step=run",
            )
            .await;
        assert_eq!(run.status, StatusCode::SEE_OTHER, "{}", run.body);
        let jobs: i64 =
            sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'tags.mass_update'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(jobs, 1);
        let listed = app.get("/moderation/mass-edit", Some(&moderator)).await;
        assert!(
            listed.body.contains("+animal") && listed.body.contains("queued"),
            "{}",
            listed.body
        );
    }
}
