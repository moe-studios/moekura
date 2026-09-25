//! Favorite groups: users' own named, ordered lists of posts.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::pools::{self as pool_names, MAX_POSTS, PoolName};
use moekura_core::posts::PostStatus;
use moekura_db::favorite_groups::{self, Contents, Group, SaveError};
use moekura_db::{posts, users};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::visibility;
use crate::templates::{search_url, url_value};

/// Most groups one user may have.
pub const MAX_GROUPS: i64 = 100;

/// Posts per page of a group.
const POSTS_PER_PAGE: i64 = 40;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/favorite_groups", get(index).post(create))
        .route("/favorite_groups/new", get(new_form))
        .route("/favorite_groups/{id}", get(show))
        .route("/favorite_groups/{id}/edit", get(edit_form).post(edit))
        .route("/favorite_groups/{id}/delete", post(delete))
        .route("/posts/{id}/favorite_groups", post(add_post))
        .route("/posts/{id}/favorite_groups/remove", post(remove_post))
}

fn group_url(id: i32) -> String {
    format!("/favorite_groups/{id}")
}

fn me(current: &CurrentUser) -> Option<i64> {
    current.user.as_ref().map(|u| u.id)
}

/// Group `id`, if `current` may see it: public, or theirs.
pub(crate) async fn visible_group(
    db: &sqlx::PgPool,
    current: &CurrentUser,
    id: i32,
) -> Result<Group, AppError> {
    current.require(Permission::ViewPosts)?;
    favorite_groups::by_id(db, id)
        .await?
        .filter(|g| g.is_public || Some(g.creator_id) == me(current))
        .ok_or(AppError::NotFound)
}

/// Group `id`, if it's `current`'s own and they may change it.
pub(crate) async fn own_group(
    db: &sqlx::PgPool,
    current: &CurrentUser,
    id: i32,
) -> Result<Group, AppError> {
    current.require(Permission::Favorite)?;
    let user = me(current).ok_or(AppError::Unauthorized)?;
    let group = favorite_groups::by_id(db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if group.creator_id != user {
        return Err(if group.is_public {
            AppError::Forbidden
        } else {
            AppError::NotFound
        });
    }
    Ok(group)
}

fn summary_context(group: &Group) -> Value {
    context! {
        id => group.id,
        name => PoolName::display(&group.name),
        url => url_value(&group_url(group.id)),
        creator => group.creator_name,
        public => group.is_public,
        post_count => group.post_count,
        date => group.updated_at.date().to_string(),
    }
}

/// Checks a group's fields, from a form or the API.
pub(crate) fn contents(
    name: &str,
    is_public: bool,
    post_ids: Vec<i64>,
) -> Result<Contents, AppError> {
    let name =
        PoolName::parse(name).map_err(|e| AppError::Unprocessable(format!("The name {e}.")))?;
    if post_ids.len() > MAX_POSTS {
        return Err(AppError::Unprocessable(format!(
            "A group can have at most {MAX_POSTS} posts."
        )));
    }
    Ok(Contents {
        name: name.as_str().to_owned(),
        is_public,
        post_ids,
    })
}

pub(crate) fn save_error(error: SaveError) -> AppError {
    match error {
        SaveError::NameTaken => {
            AppError::Unprocessable("You already have a group with that name.".into())
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
    /// Whose groups; the viewer's own by default.
    #[serde(default)]
    user: String,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let owner = if query.user.is_empty() {
        let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
        user.clone()
    } else {
        users::by_name(db, &query.user)
            .await?
            .ok_or(AppError::NotFound)?
    };
    let own = me(&page.current) == Some(owner.id);
    let groups = favorite_groups::for_user(db, owner.id, own).await?;
    Ok(page.render(
        "favorite_groups.html",
        context! {
            owner => owner.name,
            own => own,
            groups => groups.iter().map(summary_context).collect::<Vec<_>>(),
            can_create => own && page.current.can(Permission::Favorite),
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
    let db = page.state().reader(&page.current);
    let group = visible_group(db, &page.current, id).await?;
    let number = query.page.unwrap_or(1).max(1);
    let mut ids = favorite_groups::visible_post_ids(
        db,
        id,
        &visibility(&page.current),
        (number - 1) * POSTS_PER_PAGE,
        POSTS_PER_PAGE + 1,
    )
    .await?;
    let has_next = ids.len() > POSTS_PER_PAGE as usize;
    ids.truncate(POSTS_PER_PAGE as usize);
    let cards: Vec<Value> = crate::posts::grid(&page, db, &ids, None)
        .await?
        .into_iter()
        .map(|(_, card)| card)
        .collect();
    let page_url = |n: i64| url_value(&format!("/favorite_groups/{id}?page={n}"));
    let own = me(&page.current) == Some(group.creator_id);
    Ok(page.render(
        "favorite_group.html",
        context! {
            group => summary_context(&group),
            cards => cards,
            owner_url => url_value(&format!(
                "/favorite_groups?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("user", &group.creator_name)
                    .finish()
            )),
            search_url => Value::from_safe_string(search_url(&format!("favgroup:{id}"))),
            can_edit => own && page.current.can(Permission::Favorite),
            previous_url => (number > 1).then(|| page_url(number - 1)),
            next_url => has_next.then(|| page_url(number + 1)),
        },
    ))
}

async fn edit_context(
    page: &Page,
    group: Option<&Group>,
    name: &str,
    is_public: bool,
    posts: &str,
    error: Option<String>,
) -> Result<Value, AppError> {
    let ids = pool_names::parse_post_ids(posts).unwrap_or_default();
    let shown: Vec<i64> = ids.into_iter().take(500).collect();
    let cards: Vec<Value> = crate::posts::grid(page, page.state().db.primary(), &shown, None)
        .await?
        .into_iter()
        .map(|(_, card)| card)
        .collect();
    Ok(context! {
        group => group.map(summary_context),
        action => url_value(&match group {
            Some(group) => format!("/favorite_groups/{}/edit", group.id),
            None => "/favorite_groups".to_owned(),
        }),
        name => name,
        public => is_public,
        posts => posts,
        cards => cards,
        error => error,
    })
}

async fn new_form(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::Favorite)?;
    let context = edit_context(&page, None, "", true, "", None).await?;
    Ok(page.render("favorite_group_edit.html", context))
}

#[derive(Debug, Deserialize)]
struct GroupForm {
    name: String,
    /// A checkbox: present when ticked.
    #[serde(default)]
    public: Option<String>,
    #[serde(default)]
    posts: String,
}

fn form_contents(form: &GroupForm) -> Result<Contents, AppError> {
    let post_ids = pool_names::parse_post_ids(&form.posts)
        .map_err(|word| AppError::Unprocessable(format!("“{word}” isn't a post number.")))?;
    contents(&form.name, form.public.is_some(), post_ids)
}

async fn refused(
    page: &Page,
    group: Option<&Group>,
    form: &GroupForm,
    error: AppError,
) -> Result<Response, AppError> {
    let status = error.status();
    let context = edit_context(
        page,
        group,
        &form.name,
        form.public.is_some(),
        &form.posts,
        Some(error.public_message().to_owned()),
    )
    .await?;
    Ok(page.render_with_status(status, "favorite_group_edit.html", context))
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<GroupForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::Favorite)?;
    let user = me(&page.current).ok_or(AppError::Unauthorized)?;
    let db = page.state().db.primary();
    let contents = match form_contents(&form) {
        Ok(contents) => contents,
        Err(error) => return refused(&page, None, &form, error).await,
    };
    if favorite_groups::count_for_user(db, user).await? >= MAX_GROUPS {
        let error = AppError::Unprocessable(format!("You can have at most {MAX_GROUPS} groups."));
        return refused(&page, None, &form, error).await;
    }
    match favorite_groups::create(db, user, &contents).await {
        Ok(id) => Ok((flash::set(jar, Flash::Saved), Redirect::to(&group_url(id))).into_response()),
        Err(error) => refused(&page, None, &form, save_error(error)).await,
    }
}

async fn edit_form(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    let db = page.state().db.primary();
    let group = own_group(db, &page.current, id).await?;
    let posts = favorite_groups::post_ids(db, id)
        .await?
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let context = edit_context(
        &page,
        Some(&group),
        &group.name,
        group.is_public,
        &posts,
        None,
    )
    .await?;
    Ok(page.render("favorite_group_edit.html", context))
}

async fn edit(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<GroupForm>,
) -> Result<Response, AppError> {
    let db = page.state().db.primary();
    let group = own_group(db, &page.current, id).await?;
    let contents = match form_contents(&form) {
        Ok(contents) => contents,
        Err(error) => return refused(&page, Some(&group), &form, error).await,
    };
    match favorite_groups::save(db, id, &contents).await {
        Ok(()) => Ok((flash::set(jar, Flash::Saved), Redirect::to(&group_url(id))).into_response()),
        Err(error) => refused(&page, Some(&group), &form, save_error(error)).await,
    }
}

async fn delete(page: Page, jar: CookieJar, Path(id): Path<i32>) -> Result<Response, AppError> {
    let db = page.state().db.primary();
    own_group(db, &page.current, id).await?;
    favorite_groups::delete(db, id).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to("/favorite_groups"),
    )
        .into_response())
}

/// Adds post `post_id` to the end of `current`'s group `id`.
pub(crate) async fn append(
    state: &AppState,
    current: &CurrentUser,
    id: i32,
    post_id: i64,
) -> Result<(), AppError> {
    let db = state.db.primary();
    own_group(db, current, id).await?;
    let post = posts::by_id(db, post_id).await?.ok_or(AppError::NotFound)?;
    if !visibility(current).allows(&post) || post.status == PostStatus::Deleted {
        return Err(AppError::NotFound);
    }
    if favorite_groups::post_ids(db, id).await?.len() >= MAX_POSTS {
        return Err(AppError::Unprocessable(format!(
            "A group can have at most {MAX_POSTS} posts."
        )));
    }
    favorite_groups::append(db, id, post_id).await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct PostForm {
    group: i32,
}

async fn add_post(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<PostForm>,
) -> Result<Response, AppError> {
    append(page.state(), &page.current, form.group, id).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/posts/{id}")),
    )
        .into_response())
}

async fn remove_post(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<PostForm>,
) -> Result<Response, AppError> {
    let db = page.state().db.primary();
    own_group(db, &page.current, form.group).await?;
    favorite_groups::remove_post(db, form.group, id).await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/posts/{id}")),
    )
        .into_response())
}

/// The viewer's groups on a post page: those holding the post, and those
/// it can be added to.
pub(crate) async fn for_post(
    state: &AppState,
    current: &CurrentUser,
    post_id: i64,
) -> Result<Option<Value>, AppError> {
    let Some(user) = me(current).filter(|_| current.can(Permission::Favorite)) else {
        return Ok(None);
    };
    let db = state.db.primary();
    let groups = favorite_groups::for_user(db, user, true).await?;
    let containing = favorite_groups::containing(db, user, post_id).await?;
    let (holding, others): (Vec<&Group>, Vec<&Group>) =
        groups.iter().partition(|g| containing.contains(&g.id));
    let brief = |g: &&Group| {
        context! { id => g.id, name => PoolName::display(&g.name), url => url_value(&group_url(g.id)) }
    };
    Ok(Some(context! {
        holding => holding.iter().map(brief).collect::<Vec<_>>(),
        others => others.iter().map(brief).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    async fn post(pool: &PgPool) -> i64 {
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
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
    async fn groups_are_their_owners(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            routes()
                .merge(crate::posts::routes())
                .merge(crate::users::routes()),
        );
        let (a, b) = (post(&pool).await, post(&pool).await);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;

        let created = app
            .post_form(
                "/favorite_groups",
                Some(&alice),
                &[],
                &format!("name=Best+Ones&public=1&posts={b}"),
            )
            .await;
        assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
        let url = created.location.unwrap();
        let id: i32 = url.rsplit('/').next().unwrap().parse().unwrap();
        let secret = app
            .post_form("/favorite_groups", Some(&alice), &[], "name=Secret")
            .await
            .location
            .unwrap();
        let taken = app
            .post_form("/favorite_groups", Some(&alice), &[], "name=best_ones")
            .await;
        assert_eq!(taken.status, StatusCode::UNPROCESSABLE_ENTITY);

        // Add from the post page.
        let page = app.get(&format!("/posts/{a}"), Some(&alice)).await;
        assert!(
            page.body
                .contains(&format!("<option value=\"{id}\">Best Ones</option>")),
            "{}",
            page.body
        );
        let added = app
            .post_form(
                &format!("/posts/{a}/favorite_groups"),
                Some(&alice),
                &[],
                &format!("group={id}"),
            )
            .await;
        assert_eq!(added.status, StatusCode::SEE_OTHER);
        assert_eq!(favorite_groups::post_ids(&pool, id).await.unwrap(), [b, a]);
        assert!(
            app.get(&format!("/posts/{a}"), Some(&alice))
                .await
                .body
                .contains("aria-label=\"Remove from Best Ones\"")
        );
        // Not to someone else's group.
        assert_eq!(
            app.post_form(
                &format!("/posts/{a}/favorite_groups"),
                Some(&bob),
                &[],
                &format!("group={id}")
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );

        // Public groups for everyone, private ones only for their owner.
        assert_eq!(app.get(&url, None).await.status, StatusCode::OK);
        assert_eq!(
            app.get(&secret, Some(&bob)).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(app.get(&secret, Some(&alice)).await.status, StatusCode::OK);
        let listed = app.get("/favorite_groups?user=alice", Some(&bob)).await;
        assert!(
            listed.body.contains("Best Ones") && !listed.body.contains("Secret"),
            "{}",
            listed.body
        );
        assert!(
            app.get("/favorite_groups", Some(&alice))
                .await
                .body
                .contains("Secret")
        );
        assert!(
            app.get("/users/alice", None)
                .await
                .body
                .contains("href=\"/favorite_groups?user=alice\"")
        );

        // Owners edit and delete.
        assert_eq!(
            app.post_form(&format!("{url}/edit"), Some(&bob), &[], "name=Mine")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        app.post_form(
            &format!("{url}/edit"),
            Some(&alice),
            &[],
            &format!("name=Best+Ones&posts={a}+{b}"),
        )
        .await;
        let group = favorite_groups::by_id(&pool, id).await.unwrap().unwrap();
        assert!(!group.is_public, "unticked");
        assert_eq!(favorite_groups::post_ids(&pool, id).await.unwrap(), [a, b]);
        app.post(&format!("{url}/delete"), Some(&alice), &[]).await;
        assert!(favorite_groups::by_id(&pool, id).await.unwrap().is_none());
    }
}
