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
use moekura_core::post_edit::{self, Metatag};
use moekura_core::posts::Rating;
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

/// A mass edit, checked: the search normalised, the tags to add and
/// remove, and the rating to set.
pub(crate) struct Checked {
    pub query: String,
    pub add: Vec<String>,
    pub remove: Vec<String>,
    pub rating: Option<Rating>,
}

/// What the tags to add ask for.
struct Changes {
    add: Vec<String>,
    remove: Vec<String>,
    rating: Option<Rating>,
}

/// The tags to add, as typed: also `-tag` to remove one and `rating:x`,
/// the metatags that make sense for many posts at once.
fn changes(text: &str) -> Result<Changes, AppError> {
    let mut add = Vec::new();
    let mut remove = Vec::new();
    let mut rating = None;
    for word in text.split_whitespace() {
        let parsed = post_edit::parse(word, &[], &[]);
        if let Some(bad) = parsed.bad_metatags.first() {
            return Err(AppError::Unprocessable(format!("{bad}.")));
        }
        match parsed.metatags.first() {
            Some(Metatag::Rating(r)) => rating = Some(*r),
            Some(_) => {
                return Err(AppError::Unprocessable(format!(
                    "“{word}” can't be used in a mass edit; only tags, -tags and rating: can."
                )));
            }
            None => {
                let (list, name) = match word.strip_prefix('-') {
                    Some(rest) if !parsed.removed.is_empty() => (&mut remove, rest),
                    _ => (&mut add, word),
                };
                for name in tag_list(name)? {
                    if !list.contains(&name) {
                        list.push(name);
                    }
                }
            }
        }
    }
    Ok(Changes {
        add,
        remove,
        rating,
    })
}

pub(crate) fn check(query: &str, add: &str, remove: &str) -> Result<Checked, AppError> {
    let parsed = SearchQuery::parse(query).map_err(|e| AppError::Unprocessable(format!("{e}.")))?;
    if parsed.is_empty() {
        return Err(AppError::Unprocessable(
            "Give a search: a mass edit of every post isn't allowed.".into(),
        ));
    }
    let removing = remove;
    let Changes {
        add,
        mut remove,
        rating,
    } = changes(add)?;
    for name in tag_list(&remove_text(removing))? {
        if !remove.contains(&name) {
            remove.push(name);
        }
    }
    if add.is_empty() && remove.is_empty() && rating.is_none() {
        return Err(AppError::Unprocessable(
            "Give tags to add or remove, or a rating.".into(),
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
        rating,
    })
}

/// The remove box, where a `-` is allowed but not needed.
fn remove_text(text: &str) -> String {
    text.split_whitespace()
        .map(|w| w.strip_prefix('-').unwrap_or(w))
        .collect::<Vec<_>>()
        .join(" ")
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
    let id = mass_updates::create(
        &mut *tx,
        actor,
        &edit.query,
        &edit.add,
        &edit.remove,
        edit.rating,
    )
    .await?;
    moekura_db::jobs::enqueue(&mut tx, &MassUpdate { id }).await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::MassUpdate).details(serde_json::json!({
            "id": id,
            "query": edit.query,
            "add": edit.add,
            "remove": edit.remove,
            "rating": edit.rating.map(Rating::code),
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
        rating => u.rating.as_deref().and_then(|r| r.parse::<Rating>().ok()).map(Rating::label),
        status => u.status,
        seen => u.seen,
        changed => u.changed,
        error => u.error,
        when => crate::dates::day(u.created_at),
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
        rating => checked.rating.map(Rating::label),
    };
    let form = MassEditForm {
        query: checked.query.clone(),
        add: checked
            .add
            .iter()
            .cloned()
            .chain(checked.rating.map(|r| format!("rating:{}", r.code())))
            .collect::<Vec<_>>()
            .join(" "),
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

    #[test]
    fn checks_metatags() {
        let checked = super::check("cat", "dog -cute rating:e dog", "-old").unwrap();
        assert_eq!(checked.add, ["dog"]);
        assert_eq!(checked.remove, ["cute", "old"]);
        assert_eq!(checked.rating, Some(moekura_core::posts::Rating::Explicit));
        assert!(super::check("cat", "rating:q", "").is_ok());
        for (add, error) in [
            ("pool:3", "can't be used in a mass edit"),
            ("rating:x", "isn't a rating"),
            ("", "or a rating"),
        ] {
            match super::check("cat", add, "") {
                Err(AppError::Unprocessable(message)) => {
                    assert!(message.contains(error), "{add}: {message}");
                }
                _ => panic!("{add} should be refused"),
            }
        }
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
