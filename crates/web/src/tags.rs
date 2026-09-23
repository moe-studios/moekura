//! The tag list, tag editing, and turning a tag input box into tags.

use axum::extract::{Path, Query, State};
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Json, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uwuu_core::permissions::Permission;
use uwuu_core::tags::{InvalidTag, POST_MAX_TAGS, TagInput, TagName, parse_input};
use uwuu_db::tags::{self, Category, ListOrder, Tag, WantedTag};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::{search_url, url_value};

/// Tags per page of the tag list.
const PAGE_SIZE: i64 = 50;
/// Deepest page of the tag list; narrow the pattern to see further.
const MAX_PAGE: i64 = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/tags", get(index))
        .route("/tags/autocomplete", get(autocomplete))
        .route("/tags/{id}/edit", get(edit_form).post(edit))
}

/// Tags from an input box, validated but not yet created.
#[derive(Debug, Default)]
pub struct ParsedTags {
    tags: Vec<(TagName, Option<i16>)>,
}

impl ParsedTags {
    pub fn wanted(&self) -> Vec<WantedTag<'_>> {
        self.tags
            .iter()
            .map(|(name, category_id)| WantedTag {
                name: name.as_str(),
                category_id: *category_id,
            })
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TagFieldError {
    /// Shown to the user on the form.
    #[error("{0}")]
    Invalid(String),
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
}

/// Parses a tag input box: whitespace-separated names, optionally with a
/// category prefix. Rejects invalid names, too many tags and deprecated
/// tags.
pub async fn parse_field(db: &PgPool, input: &str) -> Result<ParsedTags, TagFieldError> {
    let categories = tags::categories(db).await?;
    let names: Vec<&str> = categories.iter().map(|c| c.name.as_str()).collect();
    let (inputs, invalid) = parse_input(input, &names);
    reject_invalid(&invalid)?;
    if inputs.len() > POST_MAX_TAGS {
        return Err(too_many());
    }
    checked(db, &categories, inputs).await
}

/// Tag changes from an edit form.
#[derive(Debug, Default)]
pub struct TagEdit {
    pub added: ParsedTags,
    /// Names taken out.
    pub removed: Vec<String>,
}

/// Compares the tags an edit form started with (`old`) with what was
/// submitted (`new`). Only tags added in the form are checked, so a tag
/// deprecated since it was added doesn't block other edits.
pub async fn parse_edit(db: &PgPool, old: &str, new: &str) -> Result<TagEdit, TagFieldError> {
    let categories = tags::categories(db).await?;
    let names: Vec<&str> = categories.iter().map(|c| c.name.as_str()).collect();
    let (before, _) = parse_input(old, &names);
    let (after, invalid) = parse_input(new, &names);
    reject_invalid(&invalid)?;
    let removed = before
        .iter()
        .filter(|b| !after.iter().any(|a| a.name == b.name))
        .map(|b| b.name.to_string())
        .collect();
    let added = after
        .into_iter()
        .filter(|a| !before.iter().any(|b| b.name == a.name))
        .collect();
    Ok(TagEdit {
        added: checked(db, &categories, added).await?,
        removed,
    })
}

pub fn too_many() -> TagFieldError {
    TagFieldError::Invalid(format!("A post can have at most {POST_MAX_TAGS} tags."))
}

fn reject_invalid(invalid: &[InvalidTag]) -> Result<(), TagFieldError> {
    if invalid.is_empty() {
        return Ok(());
    }
    let list: Vec<String> = invalid.iter().map(ToString::to_string).collect();
    Err(TagFieldError::Invalid(format!(
        "Some tags aren't valid: {}.",
        list.join("; ")
    )))
}

/// Refuses deprecated tags and resolves category prefixes.
async fn checked(
    db: &PgPool,
    categories: &[Category],
    inputs: Vec<TagInput>,
) -> Result<ParsedTags, TagFieldError> {
    let lookup: Vec<&str> = inputs.iter().map(|t| t.name.as_str()).collect();
    let mut deprecated: Vec<String> = tags::by_names(db, &lookup)
        .await?
        .into_iter()
        .filter(|t| t.is_deprecated)
        .map(|t| format!("`{}`", t.name))
        .collect();
    if !deprecated.is_empty() {
        deprecated.sort();
        return Err(TagFieldError::Invalid(format!(
            "These tags are deprecated and can't be added: {}.",
            deprecated.join(", ")
        )));
    }
    let category_id = |name: &str| categories.iter().find(|c| c.name == name).map(|c| c.id);
    Ok(ParsedTags {
        tags: inputs
            .into_iter()
            .map(|input| {
                let category = input.category.as_deref().and_then(category_id);
                (input.name, category)
            })
            .collect(),
    })
}

fn tag_context(tag: &Tag) -> Value {
    context! {
        id => tag.id,
        name => tag.name,
        count => tag.post_count,
        deprecated => tag.is_deprecated,
        url => Value::from_safe_string(search_url(&tag.name)),
    }
}

/// `tags` grouped by category in display order, sorted by name within
/// each group; empty groups are left out.
pub fn grouped(categories: &[Category], mut tags: Vec<Tag>) -> Vec<Value> {
    tags.sort_by(|a, b| a.name.cmp(&b.name));
    categories
        .iter()
        .filter_map(|category| {
            let members: Vec<Value> = tags
                .iter()
                .filter(|t| t.category_id == category.id)
                .map(tag_context)
                .collect();
            (!members.is_empty()).then(|| {
                context! {
                    name => category.name,
                    label => category.label,
                    tags => members,
                }
            })
        })
        .collect()
}

/// Suggestions per autocomplete request.
const SUGGESTIONS: i64 = 10;

#[derive(Debug, Deserialize)]
struct AutocompleteQuery {
    #[serde(default)]
    q: String,
}

#[derive(Debug, Serialize)]
struct Suggestion {
    name: String,
    category: String,
    post_count: i32,
    /// The alias that matched, when `name` is its target.
    #[serde(skip_serializing_if = "Option::is_none")]
    antecedent: Option<String>,
}

/// Tag suggestions as JSON, for the autocomplete script.
async fn autocomplete(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<AutocompleteQuery>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.db.read();
    let prefix = uwuu_core::tags::normalize(&query.q);
    let found = tags::autocomplete(db, &prefix, SUGGESTIONS).await?;
    let categories = tags::categories(db).await?;
    let suggestions: Vec<Suggestion> = found
        .into_iter()
        .map(|s| Suggestion {
            category: categories
                .iter()
                .find(|c| c.id == s.category_id)
                .map_or_else(|| "general".to_owned(), |c| c.name.clone()),
            name: s.name,
            post_count: s.post_count,
            antecedent: s.antecedent,
        })
        .collect();
    // Private: what a viewer may see depends on their session.
    Ok(([(CACHE_CONTROL, "private, max-age=60")], Json(suggestions)).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    name: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    order: String,
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().db.read();
    let categories = tags::categories(db).await?;
    let category = categories.iter().find(|c| c.name == query.category);
    let order = match query.order.as_str() {
        "name" => ListOrder::Name,
        "newest" => ListOrder::Newest,
        _ => ListOrder::Count,
    };
    let number = query.page.unwrap_or(1).clamp(1, MAX_PAGE);
    let pattern = uwuu_core::tags::normalize(&query.name);
    let mut found = tags::list(
        db,
        &pattern,
        category.map(|c| c.id),
        order,
        (number - 1) * PAGE_SIZE,
        PAGE_SIZE + 1,
    )
    .await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);

    let label = |id: i16| categories.iter().find(|c| c.id == id);
    let rows: Vec<Value> = found
        .iter()
        .map(|tag| {
            let category = label(tag.category_id);
            context! {
                ..tag_context(tag),
                ..context! {
                    category => category.map(|c| c.name.clone()),
                    category_label => category.map(|c| c.label.clone()),
                }
            }
        })
        .collect();
    let page_url = |n: i64| {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", &query.name)
            .append_pair("category", &query.category)
            .append_pair("order", &query.order)
            .append_pair("page", &n.to_string())
            .finish();
        url_value(&format!("/tags?{query}"))
    };
    Ok(page.render(
        "tags.html",
        context! {
            tags => rows,
            categories => categories.iter().map(|c| context! { name => c.name, label => c.label }).collect::<Vec<_>>(),
            query => context! { name => query.name, category => query.category, order => query.order },
            can_edit => page.current.can(Permission::ManageTags),
            previous_url => (number > 1).then(|| page_url(number - 1)),
            next_url => has_next.then(|| page_url(number + 1)),
        },
    ))
}

async fn edit_form(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    page.current.require(Permission::ManageTags)?;
    let db = page.state().db.primary();
    let tag = tags::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    let categories = tags::categories(db).await?;
    Ok(page.render(
        "tag_edit.html",
        context! {
            tag => tag_context(&tag),
            category_id => tag.category_id,
            categories => categories.iter().map(|c| context! { id => c.id, label => c.label }).collect::<Vec<_>>(),
        },
    ))
}

#[derive(Debug, Deserialize)]
struct EditForm {
    category: i16,
    /// Present (as "on") when the box is ticked.
    deprecated: Option<String>,
}

async fn edit(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<EditForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageTags)?;
    let db = page.state().db.primary();
    let tag = tags::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !tags::categories(db)
        .await?
        .iter()
        .any(|c| c.id == form.category)
    {
        return Err(AppError::BadRequest("Unknown category".into()));
    }
    tags::update(db, id, form.category, form.deprecated.is_some()).await?;
    tracing::info!(tag = tag.name, category = form.category, "tag edited");
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("name", &tag.name)
        .finish();
    let back = format!("/tags?{query}");
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&back)).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use uwuu_core::permissions::SystemRole;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    async fn parse(pool: &PgPool, input: &str) -> Result<Vec<(String, Option<i16>)>, String> {
        match parse_field(pool, input).await {
            Ok(parsed) => Ok(parsed
                .tags
                .into_iter()
                .map(|(name, category)| (name.into_string(), category))
                .collect()),
            Err(TagFieldError::Invalid(message)) => Err(message),
            Err(TagFieldError::Db(e)) => panic!("{e}"),
        }
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn parses_tag_fields(pool: PgPool) {
        assert_eq!(
            parse(&pool, "Long_Hair artist:Someone copyright:x:y")
                .await
                .unwrap(),
            [
                ("long_hair".to_owned(), None),
                ("someone".to_owned(), Some(1)),
                ("x:y".to_owned(), Some(3)),
            ]
        );
        let error = parse(&pool, "ok *bad* rating:e").await.unwrap_err();
        assert_eq!(
            error,
            "Some tags aren't valid: `*bad*` may not contain `*`; `rating:e` may not start with `rating:`."
        );
        let many: String = (0..=POST_MAX_TAGS).map(|i| format!("t{i} ")).collect();
        assert!(parse(&pool, &many).await.unwrap_err().contains("at most"));

        sqlx::query("INSERT INTO tags (name, is_deprecated) VALUES ('old', true)")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            parse(&pool, "fine old").await.unwrap_err(),
            "These tags are deprecated and can't be added: `old`."
        );
    }

    /// The autocomplete script keeps its own list of metatags.
    #[test]
    fn script_knows_the_metatags() {
        let script = include_str!("../../../frontend/src/metatags.ts");
        for name in uwuu_core::search::METATAGS {
            assert!(script.contains(&format!("  {name}: [")), "{name}");
        }
        for (name, order) in uwuu_core::search::Order::NAMES {
            if order.name() == *name {
                assert!(script.contains(&format!("\"{name}\"")), "order:{name}");
            }
        }
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn autocomplete_returns_json(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, routes());
        let id: i32 = sqlx::query_scalar(
            "INSERT INTO tags (name, category_id) VALUES ('someone', 1) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO posts (rating, tag_ids) VALUES ('g', ARRAY[$1])")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let response = app.get("/tags/autocomplete?q=Some", None).await;
        assert_eq!(response.status, StatusCode::OK);
        let json: serde_json::Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(
            json,
            serde_json::json!([{ "name": "someone", "category": "artist", "post_count": 1 }])
        );
        let empty = app.get("/tags/autocomplete?q=", None).await;
        assert_eq!(empty.body, "[]");
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn edits_only_check_added_tags(pool: PgPool) {
        sqlx::query("INSERT INTO tags (name, is_deprecated) VALUES ('old', true)")
            .execute(&pool)
            .await
            .unwrap();
        let edit = parse_edit(&pool, "old keep gone", "keep old artist:new")
            .await
            .unwrap();
        assert_eq!(edit.removed, ["gone"]);
        let added: Vec<_> = edit
            .added
            .tags
            .iter()
            .map(|(n, c)| (n.as_str(), *c))
            .collect();
        assert_eq!(added, [("new", Some(1))]);
        assert!(matches!(
            parse_edit(&pool, "keep", "keep old").await,
            Err(TagFieldError::Invalid(m)) if m.contains("deprecated")
        ));
    }

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
    async fn tag_list_and_editing(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, routes());
        let id: i32 =
            sqlx::query_scalar("INSERT INTO tags (name) VALUES ('fate/stay_night') RETURNING id")
                .fetch_one(&pool)
                .await
                .unwrap();
        let list = app.get("/tags?name=fate", None).await;
        assert_eq!(list.status, StatusCode::OK);
        assert!(
            list.body.contains("href=\"/posts?tags=fate%2Fstay_night\""),
            "{}",
            list.body
        );
        assert!(!list.body.contains("/edit"), "no edit links for visitors");

        let member = session_for(&pool, "alice", SystemRole::Member).await;
        let edit = format!("/tags/{id}/edit");
        assert_eq!(
            app.get(&edit, Some(&member)).await.status,
            StatusCode::FORBIDDEN
        );
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        assert!(app.get("/tags", Some(&admin)).await.body.contains(&edit));
        assert_eq!(app.get(&edit, Some(&admin)).await.status, StatusCode::OK);

        let response = app
            .post_form(&edit, Some(&admin), &[], "category=3&deprecated=on")
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        assert_eq!(
            response.location.as_deref(),
            Some("/tags?name=fate%2Fstay_night")
        );
        let tag = tags::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!((tag.category_id, tag.is_deprecated), (3, true));

        let bad = app.post_form(&edit, Some(&admin), &[], "category=99").await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    }
}
