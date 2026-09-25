//! Pools: ordered collections of posts, with history.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_core::pools::{self as pool_names, Category, MAX_POSTS, PoolName};
use moekura_core::posts::PostStatus;
use moekura_core::search::PoolRef;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::pools::{self, Contents, Pool, SaveError};
use moekura_db::posts;
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::visibility;
use crate::tags::{MAX_PAGE, PAGE_SIZE};
use crate::templates::{search_url, url_value};

/// Posts per page of a pool.
const POSTS_PER_PAGE: i64 = 40;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/pools", get(index).post(create))
        .route("/pools/new", get(new_form))
        .route("/pools/{id}", get(show))
        .route("/pools/{id}/edit", get(edit_form).post(edit))
        .route("/pools/{id}/history", get(history))
        .route("/pools/{id}/revert/{version}", post(revert))
        .route("/pools/{id}/delete", post(delete))
        .route("/pools/{id}/undelete", post(undelete))
        .route("/posts/{id}/pools", post(add_post))
}

pub(crate) fn pool_url(id: i32) -> String {
    format!("/pools/{id}")
}

/// Whether `current` sees deleted pools.
fn sees_deleted(current: &CurrentUser) -> bool {
    current.can(Permission::DeletePosts) || current.can(Permission::ViewDeleted)
}

/// Pool `id`, if `current` may see it.
pub(crate) async fn visible_pool(
    db: &sqlx::PgPool,
    current: &CurrentUser,
    id: i32,
) -> Result<Pool, AppError> {
    current.require(Permission::ViewPosts)?;
    let pool = pools::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if pool.is_deleted && !sees_deleted(current) {
        return Err(AppError::NotFound);
    }
    Ok(pool)
}

/// The pool a user typed, by name or id.
pub(crate) async fn find_typed(
    db: &sqlx::PgPool,
    current: &CurrentUser,
    typed: &str,
) -> Result<Pool, AppError> {
    let typed = typed.trim().trim_start_matches('#');
    let reference = match typed.parse::<i32>() {
        Ok(id) => PoolRef::Id(id),
        Err(_) => PoolRef::Name(typed.split_whitespace().collect::<Vec<_>>().join("_")),
    };
    let pool = pools::find(db, &reference)
        .await?
        .filter(|p| !p.is_deleted || sees_deleted(current))
        .ok_or_else(|| AppError::Unprocessable(format!("There's no pool called “{typed}”.")))?;
    Ok(pool)
}

fn summary_context(pool: &Pool) -> Value {
    context! {
        id => pool.id,
        name => PoolName::display(&pool.name),
        url => url_value(&pool_url(pool.id)),
        category => pool.category,
        post_count => pool.post_count,
        deleted => pool.is_deleted,
        updater => pool.updater_name,
        date => pool.updated_at.date().to_string(),
    }
}

/// A pool's fields, checked, from a form or the API.
pub(crate) struct PoolInput<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub category: &'a str,
    pub post_ids: Vec<i64>,
}

/// Checks `input` and turns it into what's saved.
pub(crate) fn contents(input: &PoolInput<'_>, is_deleted: bool) -> Result<Contents, AppError> {
    let name = PoolName::parse(input.name)
        .map_err(|e| AppError::Unprocessable(format!("The name {e}.")))?;
    let category = Category::parse(input.category)
        .ok_or_else(|| AppError::Unprocessable("A pool is a series or a collection.".into()))?;
    let description = input
        .description
        .replace("\r\n", "\n")
        .trim_end()
        .to_owned();
    if description.len() > markup::MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "The description is too long: the limit is {} characters.",
            markup::MAX_LEN
        )));
    }
    if input.post_ids.len() > MAX_POSTS {
        return Err(AppError::Unprocessable(format!(
            "A pool can have at most {MAX_POSTS} posts."
        )));
    }
    Ok(Contents {
        name: name.as_str().to_owned(),
        description,
        category: category.as_str().to_owned(),
        is_deleted,
        post_ids: input.post_ids.clone(),
    })
}

/// A save refused, as the visitor is told.
pub(crate) fn save_error(error: SaveError) -> AppError {
    match error {
        SaveError::Conflict { .. } => AppError::Conflict(
            "Someone else changed this pool while you were editing it. Check their changes, \
             then save again."
                .into(),
        ),
        SaveError::NameTaken => {
            AppError::Unprocessable("Another pool already has that name.".into())
        }
        SaveError::MissingPosts(ids) => AppError::Unprocessable(format!(
            "These posts don't exist: {}.",
            ids.iter()
                .map(|id| format!("#{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        SaveError::Db(e) => e.into(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    name: String,
    #[serde(default)]
    category: String,
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let number = query.page.unwrap_or(1).clamp(1, MAX_PAGE);
    let name = query.name.split_whitespace().collect::<Vec<_>>().join("_");
    let category = Category::parse(&query.category).map(Category::as_str);
    let filter = pools::Filter {
        name: &name,
        category,
        with_deleted: sees_deleted(&page.current),
    };
    let mut found = pools::list(db, &filter, (number - 1) * PAGE_SIZE, PAGE_SIZE + 1).await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);
    let list_url = |n: i64| {
        let params = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", &query.name)
            .append_pair("category", category.unwrap_or_default())
            .append_pair("page", &n.to_string())
            .finish();
        url_value(&format!("/pools?{params}"))
    };
    Ok(page.render(
        "pools.html",
        context! {
            pools => found.iter().map(summary_context).collect::<Vec<_>>(),
            query => context! { name => query.name, category => category },
            can_create => page.current.is_logged_in() && page.current.can(Permission::EditPools),
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => has_next.then(|| list_url(number + 1)),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct ShowQuery {
    page: Option<i64>,
}

async fn show(
    page: Page,
    Path(id): Path<i32>,
    Query(query): Query<ShowQuery>,
) -> Result<Response, AppError> {
    let state = page.state();
    let db = state.reader(&page.current);
    let pool = visible_pool(db, &page.current, id).await?;
    let number = query.page.unwrap_or(1).max(1);
    let mut ids = pools::visible_post_ids(
        db,
        id,
        &visibility(&page.current),
        (number - 1) * POSTS_PER_PAGE,
        POSTS_PER_PAGE + 1,
    )
    .await?;
    let has_next = ids.len() > POSTS_PER_PAGE as usize;
    ids.truncate(POSTS_PER_PAGE as usize);
    let cards: Vec<Value> = crate::posts::grid(&page, db, &ids)
        .await?
        .into_iter()
        .map(|(_, card)| card)
        .collect();
    let page_url = |n: i64| url_value(&format!("/pools/{id}?page={n}"));
    let staff = page.current.can(Permission::DeletePosts);
    Ok(page.render(
        "pool.html",
        context! {
            pool => summary_context(&pool),
            html => (!pool.description.is_empty())
                .then(|| Value::from_safe_string(markup::render(&pool.description))),
            version => pool.version,
            cards => cards,
            search_url => Value::from_safe_string(search_url(&format!("pool:{id}"))),
            can_edit => page.current.is_logged_in() && page.current.can(Permission::EditPools) && !pool.is_deleted,
            can_delete => staff && !pool.is_deleted,
            can_undelete => staff && pool.is_deleted,
            previous_url => (number > 1).then(|| page_url(number - 1)),
            next_url => has_next.then(|| page_url(number + 1)),
        },
    ))
}

/// The pool editor, filled with `fields`.
struct Fields<'a> {
    name: &'a str,
    description: &'a str,
    category: &'a str,
    posts: String,
}

async fn edit_context(
    page: &Page,
    pool: Option<&Pool>,
    fields: &Fields<'_>,
    base: i32,
    error: Option<String>,
) -> Result<Value, AppError> {
    // Thumbnails of the posts, in order, for reordering by dragging.
    let ids = pool_names::parse_post_ids(&fields.posts).unwrap_or_default();
    let db = page.state().db.primary();
    let shown: Vec<i64> = ids.iter().copied().take(500).collect();
    let cards: Vec<Value> = crate::posts::grid(page, db, &shown)
        .await?
        .into_iter()
        .map(|(_, card)| card)
        .collect();
    let categories: Vec<&str> = Category::ALL.iter().map(|c| c.as_str()).collect();
    Ok(context! {
        pool => pool.map(summary_context),
        action => url_value(&match pool {
            Some(pool) => format!("/pools/{}/edit", pool.id),
            None => "/pools".to_owned(),
        }),
        name => fields.name,
        description => fields.description,
        category => fields.category,
        categories => categories,
        posts => fields.posts,
        cards => cards,
        base => base,
        error => error,
        max_len => markup::MAX_LEN,
    })
}

async fn new_form(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::EditPools)?;
    let fields = Fields {
        name: "",
        description: "",
        category: Category::default().as_str(),
        posts: String::new(),
    };
    let context = edit_context(&page, None, &fields, 0, None).await?;
    Ok(page.render("pool_edit.html", context))
}

#[derive(Debug, Deserialize)]
struct PoolForm {
    name: String,
    #[serde(default)]
    description: String,
    category: String,
    #[serde(default)]
    posts: String,
    /// The version the form was loaded with; absent for a new pool.
    base: Option<i32>,
}

/// Checks the form into what's saved.
fn form_contents(form: &PoolForm, is_deleted: bool) -> Result<Contents, AppError> {
    let post_ids = pool_names::parse_post_ids(&form.posts)
        .map_err(|word| AppError::Unprocessable(format!("“{word}” isn't a post number.")))?;
    contents(
        &PoolInput {
            name: &form.name,
            description: &form.description,
            category: &form.category,
            post_ids,
        },
        is_deleted,
    )
}

/// The form again, with `error`.
async fn refused(
    page: &Page,
    pool: Option<&Pool>,
    form: &PoolForm,
    base: i32,
    error: AppError,
) -> Result<Response, AppError> {
    let fields = Fields {
        name: &form.name,
        description: &form.description,
        category: &form.category,
        posts: form.posts.clone(),
    };
    let status = error.status();
    let context = edit_context(
        page,
        pool,
        &fields,
        base,
        Some(error.public_message().to_owned()),
    )
    .await?;
    Ok(page.render_with_status(status, "pool_edit.html", context))
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<PoolForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditPools)?;
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let contents = match form_contents(&form, false) {
        Ok(contents) => contents,
        Err(error) => return refused(&page, None, &form, 0, error).await,
    };
    match pools::create(page.state().db.primary(), &contents, Some(user.id)).await {
        Ok(id) => {
            tracing::info!(pool = id, name = contents.name, "pool created");
            Ok((flash::set(jar, Flash::Saved), Redirect::to(&pool_url(id))).into_response())
        }
        Err(error) => refused(&page, None, &form, 0, save_error(error)).await,
    }
}

async fn edit_form(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    page.current.require(Permission::EditPools)?;
    let db = page.state().db.primary();
    let pool = visible_pool(db, &page.current, id).await?;
    let post_ids = pools::post_ids(db, id).await?;
    let fields = Fields {
        name: &pool.name,
        description: &pool.description,
        category: &pool.category,
        posts: post_ids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" "),
    };
    let context = edit_context(&page, Some(&pool), &fields, pool.version, None).await?;
    Ok(page.render("pool_edit.html", context))
}

async fn edit(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<PoolForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditPools)?;
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = page.state().db.primary();
    let pool = visible_pool(db, &page.current, id).await?;
    let base = form.base.unwrap_or(pool.version);
    let contents = match form_contents(&form, pool.is_deleted) {
        Ok(contents) => contents,
        Err(error) => return refused(&page, Some(&pool), &form, base, error).await,
    };
    match pools::save(db, id, &contents, Some(user.id), Some(base)).await {
        Ok(_) => Ok((flash::set(jar, Flash::Saved), Redirect::to(&pool_url(id))).into_response()),
        // Keep their changes, and let them save over the newer version
        // once they've looked at it.
        Err(error @ SaveError::Conflict { current, .. }) => {
            refused(&page, Some(&pool), &form, current, save_error(error)).await
        }
        Err(error) => refused(&page, Some(&pool), &form, base, save_error(error)).await,
    }
}

async fn history(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    let db = page.state().reader(&page.current);
    let pool = visible_pool(db, &page.current, id).await?;
    let versions = pools::versions(db, id).await?;
    let can_revert = page.current.is_logged_in() && page.current.can(Permission::EditPools);
    let rows: Vec<Value> = versions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let previous = versions.get(i + 1);
            let added: Vec<i64> = v
                .post_ids
                .iter()
                .copied()
                .filter(|id| previous.is_none_or(|p| !p.post_ids.contains(id)))
                .collect();
            let removed: Vec<i64> = previous
                .map(|p| {
                    p.post_ids
                        .iter()
                        .copied()
                        .filter(|id| !v.post_ids.contains(id))
                        .collect()
                })
                .unwrap_or_default();
            let same_posts = |p: &pools::Version| {
                let mut a = p.post_ids.clone();
                let mut b = v.post_ids.clone();
                a.sort_unstable();
                b.sort_unstable();
                a == b
            };
            context! {
                version => v.version,
                date => v.created_at.date().to_string(),
                updater => v.updater_name,
                name => previous.is_none_or(|p| p.name != v.name).then(|| PoolName::display(&v.name)),
                category => previous.is_some_and(|p| p.category != v.category).then_some(&v.category),
                description_changed => previous.is_some_and(|p| p.description != v.description),
                deleted => previous.is_some_and(|p| p.is_deleted != v.is_deleted).then_some(v.is_deleted),
                added => added,
                removed => removed,
                reordered => previous.is_some_and(|p| p.post_ids != v.post_ids && same_posts(p)),
                current => i == 0,
                can_revert => can_revert && i > 0,
            }
        })
        .collect();
    Ok(page.render(
        "pool_history.html",
        context! { pool => summary_context(&pool), versions => rows },
    ))
}

/// Saves an earlier version's name, description, category and posts as a
/// new version (leaving whether the pool is deleted alone).
async fn revert(
    page: Page,
    jar: CookieJar,
    Path((id, version)): Path<(i32, i32)>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditPools)?;
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = page.state().db.primary();
    let pool = visible_pool(db, &page.current, id).await?;
    let old = pools::version(db, id, version)
        .await?
        .ok_or(AppError::NotFound)?;
    let contents = Contents {
        is_deleted: pool.is_deleted,
        // Posts purged since are gone for good.
        post_ids: {
            let existing = posts::by_ids(db, &old.post_ids).await?;
            old.post_ids
                .iter()
                .copied()
                .filter(|id| existing.iter().any(|p| p.id == *id))
                .collect()
        },
        ..old.contents()
    };
    pools::save(db, id, &contents, Some(user.id), None)
        .await
        .map_err(save_error)?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/pools/{id}/history")),
    )
        .into_response())
}

/// Deletes or restores pool `id` as staff, logging it.
pub(crate) async fn set_deleted(
    state: &AppState,
    current: &CurrentUser,
    id: i32,
    deleted: bool,
) -> Result<(), AppError> {
    current.require(Permission::DeletePosts)?;
    let actor = current.user.as_ref().map(|u| u.id);
    let db = state.db.primary();
    let contents = pools::contents(db, id).await?.ok_or(AppError::NotFound)?;
    if contents.is_deleted == deleted {
        return Ok(());
    }
    let name = contents.name.clone();
    pools::save(
        db,
        id,
        &Contents {
            is_deleted: deleted,
            ..contents
        },
        actor,
        None,
    )
    .await
    .map_err(save_error)?;
    let kind = if deleted {
        ActionKind::PoolDelete
    } else {
        ActionKind::PoolUndelete
    };
    mod_actions::record(
        db,
        NewAction::new(actor, kind).details(serde_json::json!({ "pool_id": id, "name": name })),
    )
    .await?;
    Ok(())
}

async fn delete(page: Page, jar: CookieJar, Path(id): Path<i32>) -> Result<Response, AppError> {
    set_deleted(page.state(), &page.current, id, true).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&pool_url(id))).into_response())
}

async fn undelete(page: Page, jar: CookieJar, Path(id): Path<i32>) -> Result<Response, AppError> {
    set_deleted(page.state(), &page.current, id, false).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&pool_url(id))).into_response())
}

/// Adds post `post_id` to the end of pool `pool`.
pub(crate) async fn append(
    state: &AppState,
    current: &CurrentUser,
    pool: &Pool,
    post_id: i64,
) -> Result<(), AppError> {
    current.require(Permission::EditPools)?;
    let user = current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = state.db.primary();
    let post = posts::by_id(db, post_id).await?.ok_or(AppError::NotFound)?;
    if !visibility(current).allows(&post) {
        return Err(AppError::NotFound);
    }
    if post.status == PostStatus::Deleted {
        return Err(AppError::Unprocessable(
            "Deleted posts can't be added to pools.".into(),
        ));
    }
    if pool.is_deleted {
        return Err(AppError::Unprocessable("The pool is deleted.".into()));
    }
    let mut contents = pools::contents(db, pool.id)
        .await?
        .ok_or(AppError::NotFound)?;
    if contents.post_ids.contains(&post_id) {
        return Err(AppError::Unprocessable(format!(
            "Post #{post_id} is already in {}.",
            PoolName::display(&pool.name)
        )));
    }
    if contents.post_ids.len() >= MAX_POSTS {
        return Err(AppError::Unprocessable(format!(
            "A pool can have at most {MAX_POSTS} posts."
        )));
    }
    contents.post_ids.push(post_id);
    pools::save(db, pool.id, &contents, Some(user.id), None)
        .await
        .map_err(save_error)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct AddForm {
    /// A pool name or id.
    pool: String,
}

async fn add_post(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<AddForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditPools)?;
    let pool = find_typed(page.state().db.primary(), &page.current, &form.pool).await?;
    append(page.state(), &page.current, &pool, id).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/posts/{id}")),
    )
        .into_response())
}

/// The pools section of a post page.
pub(crate) async fn for_post(
    state: &AppState,
    current: &CurrentUser,
    post_id: i64,
    status: PostStatus,
) -> Result<Value, AppError> {
    let memberships = pools::for_post(state.db.primary(), post_id).await?;
    let rows: Vec<Value> = memberships
        .iter()
        .map(|m| {
            context! {
                id => m.id,
                name => PoolName::display(&m.name),
                url => url_value(&pool_url(m.id)),
                category => m.category,
                position => m.position + 1,
                post_count => m.post_count,
            }
        })
        .collect();
    Ok(context! {
        pools => rows,
        can_add => current.is_logged_in()
            && current.can(Permission::EditPools)
            && status != PostStatus::Deleted,
    })
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
            routes().merge(crate::posts::routes()),
        )
    }

    async fn post(pool: &PgPool, status: &str) -> i64 {
        let post: i64 =
            sqlx::query_scalar("INSERT INTO posts (rating, status) VALUES ('g', $1) RETURNING id")
                .bind(status)
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO media_assets (post_id, sha256, md5, media_type, width, height, file_size, storage_key)
             VALUES ($1, sha256($1::text::bytea), substring(sha256($1::text::bytea) FROM 1 FOR 16), 'png', 10, 10, 1, 'original/aa/aa/x.png')",
        )
        .bind(post)
        .execute(pool)
        .await
        .unwrap();
        post
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn create_edit_and_view(pool: PgPool) {
        let app = app(&pool).await;
        let a = post(&pool, "active").await;
        let b = post(&pool, "active").await;
        let hidden = post(&pool, "pending").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;

        assert_eq!(
            app.post_form("/pools", None, &[], "name=x&category=series")
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
        let bad = app
            .post_form(
                "/pools",
                Some(&alice),
                &[],
                &format!("name=My+Comic&category=series&posts={a}+nope"),
            )
            .await;
        assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            bad.body.contains("isn&#x27;t a post number"),
            "{}",
            bad.body
        );
        assert!(bad.body.contains("value=\"My Comic\""), "keeps the form");

        let created = app
            .post_form(
                "/pools",
                Some(&alice),
                &[],
                &format!("name=My+Comic&category=series&description=Part+%5Bb%5Done%5B%2Fb%5D&posts={b}+{hidden}+{a}"),
            )
            .await;
        assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
        let url = created.location.unwrap();
        let id: i32 = url.rsplit('/').next().unwrap().parse().unwrap();

        let shown = app.get(&url, None).await;
        assert_eq!(shown.status, StatusCode::OK);
        assert!(shown.body.contains("<h1>My Comic</h1>"), "{}", shown.body);
        assert!(shown.body.contains("Part <strong>one</strong>"));
        // In order, without the pending post.
        let (first, second) = (
            shown.body.find(&format!("href=\"/posts/{b}\"")).unwrap(),
            shown.body.find(&format!("href=\"/posts/{a}\"")).unwrap(),
        );
        assert!(first < second);
        assert!(!shown.body.contains(&format!("href=\"/posts/{hidden}\"")));

        let taken = app
            .post_form(
                "/pools",
                Some(&alice),
                &[],
                "name=my_comic&category=collection",
            )
            .await;
        assert!(taken.body.contains("Another pool already has that name."));

        // Edits check the version they started from.
        let edit = format!("/pools/{id}/edit");
        assert!(
            app.get(&edit, Some(&alice))
                .await
                .body
                .contains(&format!("{b} {hidden} {a}"))
        );
        let saved = app
            .post_form(
                &edit,
                Some(&alice),
                &[],
                &format!("base=1&name=My+Comic&category=series&posts={a}+{b}"),
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        let stale = app
            .post_form(
                &edit,
                Some(&alice),
                &[],
                &format!("base=1&name=Other&category=series&posts={a}"),
            )
            .await;
        assert_eq!(stale.status, StatusCode::CONFLICT);
        assert!(stale.body.contains("name=\"base\" value=\"2\""));

        let history = app.get(&format!("/pools/{id}/history"), None).await;
        assert!(history.body.contains("Version 2"), "{}", history.body);
        assert!(
            history
                .body
                .contains(&format!("−<a href=\"/posts/{hidden}\">"))
        );
        assert_eq!(
            app.post(&format!("/pools/{id}/revert/1"), Some(&alice), &[])
                .await
                .status,
            StatusCode::SEE_OTHER
        );
        assert_eq!(pools::post_ids(&pool, id).await.unwrap(), [b, hidden, a]);

        let list = app.get("/pools?name=my", None).await;
        assert!(
            list.body.contains(&format!("href=\"/pools/{id}\"")),
            "{}",
            list.body
        );
        assert!(
            app.get("/pools?category=collection", None)
                .await
                .body
                .contains("No pools found.")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn add_from_post_pages_and_delete(pool: PgPool) {
        let app = app(&pool).await;
        let a = post(&pool, "active").await;
        let b = post(&pool, "active").await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let jan = session_for(&pool, "jan", SystemRole::Janitor).await;
        let id = pools::create(
            &pool,
            &Contents {
                name: "Series".into(),
                description: String::new(),
                category: "series".into(),
                is_deleted: false,
                post_ids: vec![a],
            },
            None,
        )
        .await
        .unwrap();

        let page = app.get(&format!("/posts/{a}"), Some(&alice)).await;
        assert!(
            page.body.contains(&format!(
                "href=\"/pools/{id}\">Series</a> <span class=\"count\">1/1"
            )),
            "{}",
            page.body
        );
        let added = app
            .post_form(
                &format!("/posts/{b}/pools"),
                Some(&alice),
                &[],
                "pool=series",
            )
            .await;
        assert_eq!(added.status, StatusCode::SEE_OTHER, "{}", added.body);
        assert_eq!(pools::post_ids(&pool, id).await.unwrap(), [a, b]);
        let again = app
            .post_form(
                &format!("/posts/{b}/pools"),
                Some(&alice),
                &[],
                &format!("pool={id}"),
            )
            .await;
        assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
        let unknown = app
            .post_form(&format!("/posts/{b}/pools"), Some(&alice), &[], "pool=nope")
            .await;
        assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);

        // Only staff delete pools; then only staff see them.
        assert_eq!(
            app.post(&format!("/pools/{id}/delete"), Some(&alice), &[])
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            app.post(&format!("/pools/{id}/delete"), Some(&jan), &[])
                .await
                .status,
            StatusCode::SEE_OTHER
        );
        assert_eq!(
            app.get(&format!("/pools/{id}"), None).await.status,
            StatusCode::NOT_FOUND
        );
        assert!(
            app.get(&format!("/pools/{id}"), Some(&jan))
                .await
                .body
                .contains("This pool is deleted.")
        );
        assert!(
            !app.get(&format!("/posts/{a}"), None)
                .await
                .body
                .contains(&format!("/pools/{id}"))
        );
        let logged: String =
            sqlx::query_scalar("SELECT action FROM mod_actions ORDER BY id DESC LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(logged, "pool.delete");
        app.post(&format!("/pools/{id}/undelete"), Some(&jan), &[])
            .await;
        assert_eq!(
            app.get(&format!("/pools/{id}"), None).await.status,
            StatusCode::OK
        );
    }
}
