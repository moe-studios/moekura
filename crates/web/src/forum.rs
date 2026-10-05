//! The forum (`/forum_topics`): topics in categories, posts with votes,
//! stickied and locked topics, what each user has read, searching, and
//! moderation (hiding posts; locking, deleting and merging topics).

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_db::forum::{self, Flags, PostFilter, Topic, TopicFilter};
use moekura_db::mod_actions::{self, NewAction};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::url_value;

/// Topics per page of the list.
const TOPICS_PAGE: i64 = 40;
/// Posts per page of a topic.
pub(crate) const POSTS_PAGE: i64 = 30;
/// The longest title, in characters.
pub(crate) const TITLE_MAX_LEN: usize = 200;
/// The longest post, in characters.
pub(crate) const BODY_MAX_LEN: usize = 50_000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/forum_topics", get(index).post(create_topic))
        .route("/forum_topics/new", get(new_topic))
        .route("/forum_topics/mark_all_read", post(mark_all_read))
        .route("/forum_topics/{id}", get(show).post(update_topic))
        .route("/forum_topics/{id}/posts", post(reply))
        .route("/forum_topics/{id}/moderate", post(moderate))
        .route("/forum_topics/{id}/merge", post(merge))
        .route("/forum_posts", get(search))
        .route("/forum_posts/{id}", get(goto_post).post(edit_post))
        .route("/forum_posts/{id}/edit", get(edit_form))
        .route("/forum_posts/{id}/vote", post(vote))
        .route("/forum_posts/{id}/hide", post(hide))
        .route("/forum_posts/{id}/unhide", post(unhide))
}

/// Whether `current` moderates the forum (and sees hidden posts and
/// deleted topics).
pub(crate) fn moderates(current: &CurrentUser) -> bool {
    current.can(Permission::ModerateComments)
}

/// Who may post: those who may comment, logged in.
fn require_poster(current: &CurrentUser) -> Result<i64, AppError> {
    current.require(Permission::Comment)?;
    current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)
}

pub(crate) fn clean_title(title: &str) -> Result<String, AppError> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > TITLE_MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "A title is 1 to {TITLE_MAX_LEN} characters long."
        )));
    }
    Ok(title.to_owned())
}

pub(crate) fn clean_body(body: &str) -> Result<String, AppError> {
    let body = body.replace("\r\n", "\n").trim_end().to_owned();
    if body.trim().is_empty() {
        return Err(AppError::Unprocessable("The post is empty.".into()));
    }
    if body.chars().count() > BODY_MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "A post can be at most {BODY_MAX_LEN} characters long."
        )));
    }
    Ok(body)
}

/// Topic `id`, if `current` may see it.
pub(crate) async fn visible_topic(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<Topic, AppError> {
    current.require(Permission::ViewPosts)?;
    let viewer = current.user.as_ref().map(|u| u.id);
    let topic = forum::topic(state.db.primary(), viewer, id)
        .await?
        .ok_or(AppError::NotFound)?;
    if topic.is_deleted && !moderates(current) && topic.merged_into_id.is_none() {
        return Err(AppError::NotFound);
    }
    Ok(topic)
}

fn topic_url(id: i64) -> String {
    format!("/forum_topics/{id}")
}

fn topic_context(t: &Topic) -> Value {
    context! {
        id => t.id,
        url => url_value(&topic_url(t.id)),
        title => t.title,
        category => t.category_name,
        category_id => t.category_id,
        creator => t.creator_name,
        sticky => t.is_sticky,
        locked => t.is_locked,
        deleted => t.is_deleted,
        posts => t.post_count,
        last_poster => t.last_poster_name,
        last_posted => crate::dates::day(t.last_posted_at),
        unread => t.unread,
    }
}

/// What [`start_topic`] or [`add_post`] made, and whether the spam filter
/// held it for review.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Posted {
    pub id: i64,
    pub held: bool,
}

/// Starts a topic as `current`.
pub(crate) async fn start_topic(
    state: &AppState,
    current: &CurrentUser,
    category_id: i16,
    title: &str,
    body: &str,
) -> Result<Posted, AppError> {
    let me = require_poster(current)?;
    let title = clean_title(title)?;
    let body = clean_body(body)?;
    let db = state.db.primary();
    if !forum::categories(db)
        .await?
        .iter()
        .any(|c| c.id == category_id)
    {
        return Err(AppError::Unprocessable("Choose a category.".into()));
    }
    state.rate_limits.check_comment(me).await?;
    let held = crate::held::check(state, current, &body).await?;
    let (topic, post) =
        forum::create_topic(db, category_id, Some(me), &title, &body, held.as_deref()).await?;
    if held.is_some() {
        return Ok(Posted {
            id: topic,
            held: true,
        });
    }
    crate::notifications::notify_text(
        state,
        Some(me),
        &body,
        &format!("the forum topic “{title}”"),
        &format!("/forum_posts/{post}"),
        (&[], moekura_db::notifications::Kind::Forum),
    )
    .await;
    Ok(Posted {
        id: topic,
        held: false,
    })
}

/// Adds a post to topic `topic_id` as `current`.
pub(crate) async fn add_post(
    state: &AppState,
    current: &CurrentUser,
    topic_id: i64,
    body: &str,
) -> Result<Posted, AppError> {
    let me = require_poster(current)?;
    let topic = visible_topic(state, current, topic_id).await?;
    if topic.is_deleted {
        return Err(AppError::Unprocessable("This topic is deleted.".into()));
    }
    if topic.is_locked && !moderates(current) {
        return Err(AppError::Unprocessable("This topic is locked.".into()));
    }
    let body = clean_body(body)?;
    state.rate_limits.check_comment(me).await?;
    let db = state.db.primary();
    let held = crate::held::check(state, current, &body).await?;
    let id = forum::create_post(db, topic.id, Some(me), &body, held.as_deref()).await?;
    forum::visit(db, me, topic.id).await?;
    if held.is_none() {
        post_published(state, Some(me), id, &topic, &body).await?;
    }
    Ok(Posted {
        id,
        held: held.is_some(),
    })
}

/// Tells those concerned of a new post: those it mentions or quotes, and
/// everyone else who posted in its topic.
pub(crate) async fn post_published(
    state: &AppState,
    creator: Option<i64>,
    id: i64,
    topic: &forum::Topic,
    body: &str,
) -> Result<(), AppError> {
    let participants = forum::participants(state.db.primary(), topic.id).await?;
    crate::notifications::notify_text(
        state,
        creator,
        body,
        &format!("the forum topic “{}”", topic.title),
        &format!("/forum_posts/{id}"),
        (&participants, moekura_db::notifications::Kind::Forum),
    )
    .await;
    Ok(())
}

/// Starts the forum topic for a tag request (an alias or implication, or
/// a bulk update), in the Tags category, as its requester, and links the
/// two. Failing only gets logged: the request stands without it.
pub(crate) async fn open_request_topic(
    state: &AppState,
    current: &CurrentUser,
    title: &str,
    body: &str,
    relation_id: Option<i32>,
    request_id: Option<i32>,
) {
    let opened = async {
        let db = state.db.primary();
        let categories = forum::categories(db).await?;
        let category = categories
            .iter()
            .find(|c| c.name == "Tags")
            .or_else(|| categories.first())
            .map(|c| c.id)
            .ok_or_else(|| AppError::Internal("no forum categories".into()))?;
        let me = require_poster(current)?;
        let title: String = title.chars().take(TITLE_MAX_LEN).collect();
        let body = clean_body(body)?;
        let (topic, _) = forum::create_topic(db, category, Some(me), &title, &body, None).await?;
        forum::link_request(db, relation_id, request_id, topic).await?;
        Ok::<_, AppError>(())
    }
    .await;
    if let Err(error) = opened {
        tracing::warn!(?error, "could not start the forum topic of a tag request");
    }
}

// ---- topics -----------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    category: Option<i16>,
    #[serde(default)]
    title: String,
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.reader(&page.current);
    let viewer = page.current.user.as_ref().map(|u| u.id);
    let number = query.page.unwrap_or(1).clamp(1, 1000);
    let filter = TopicFilter {
        category_id: query.category,
        title: &query.title,
        with_deleted: false,
        ..TopicFilter::default()
    };
    let mut found = forum::topics(
        db,
        viewer,
        &filter,
        (number - 1) * TOPICS_PAGE,
        TOPICS_PAGE + 1,
    )
    .await?;
    let more = found.len() > TOPICS_PAGE as usize;
    found.truncate(TOPICS_PAGE as usize);
    let list_url = |n: i64| {
        let mut q = url::form_urlencoded::Serializer::new(String::new());
        if let Some(c) = query.category {
            q.append_pair("category", &c.to_string());
        }
        if !query.title.is_empty() {
            q.append_pair("title", &query.title);
        }
        q.append_pair("page", &n.to_string());
        url_value(&format!("/forum_topics?{}", q.finish()))
    };
    Ok(page.render(
        "forum_topics.html",
        context! {
            topics => found.iter().map(topic_context).collect::<Vec<_>>(),
            categories => forum::categories(db).await?.iter().map(|c| context! {
                id => c.id, name => c.name, description => c.description,
            }).collect::<Vec<_>>(),
            query => context! { category => query.category, title => query.title },
            can_post => page.current.is_logged_in() && page.current.can(Permission::Comment),
            logged_in => page.current.is_logged_in(),
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => more.then(|| list_url(number + 1)),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct NewTopicQuery {
    category: Option<i16>,
}

fn new_topic_context(
    categories: &[forum::Category],
    category: Option<i16>,
    title: &str,
    body: &str,
    error: Option<String>,
) -> Value {
    context! {
        categories => categories.iter().map(|c| context! { id => c.id, name => c.name }).collect::<Vec<_>>(),
        category => category,
        title => title,
        body => body,
        error => error,
    }
}

async fn new_topic(page: Page, Query(query): Query<NewTopicQuery>) -> Result<Response, AppError> {
    require_poster(&page.current)?;
    let categories = forum::categories(page.state().db.primary()).await?;
    Ok(page.render(
        "forum_topic_new.html",
        new_topic_context(&categories, query.category, "", "", None),
    ))
}

#[derive(Debug, Deserialize)]
struct TopicForm {
    #[serde(default)]
    category: i16,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

async fn create_topic(
    page: Page,
    jar: CookieJar,
    Form(form): Form<TopicForm>,
) -> Result<Response, AppError> {
    match start_topic(
        page.state(),
        &page.current,
        form.category,
        &form.title,
        &form.body,
    )
    .await
    {
        Ok(Posted { held: true, .. }) => {
            Ok((flash::set(jar, Flash::Held), Redirect::to("/forum_topics")).into_response())
        }
        Ok(Posted { id, .. }) => {
            Ok((flash::set(jar, Flash::Saved), Redirect::to(&topic_url(id))).into_response())
        }
        Err(AppError::Unprocessable(message)) => {
            let categories = forum::categories(page.state().db.primary()).await?;
            Ok(page.render_with_status(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "forum_topic_new.html",
                new_topic_context(
                    &categories,
                    Some(form.category),
                    &form.title,
                    &form.body,
                    Some(message),
                ),
            ))
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug, Default, Deserialize)]
struct ShowQuery {
    page: Option<i64>,
}

async fn show(
    page: Page,
    Path(id): Path<i64>,
    Query(query): Query<ShowQuery>,
) -> Result<Response, AppError> {
    let state = page.state();
    let topic = visible_topic(state, &page.current, id).await?;
    if let Some(into) = topic.merged_into_id.filter(|_| !moderates(&page.current)) {
        return Ok(Redirect::to(&topic_url(into)).into_response());
    }
    let db = state.db.primary();
    let staff = moderates(&page.current);
    let total = i64::from(topic.post_count);
    let last_page = ((total - 1).max(0) / POSTS_PAGE) + 1;
    let number = query
        .page
        .unwrap_or(1)
        .clamp(1, last_page.max(1) + if staff { 100 } else { 0 });
    let filter = PostFilter {
        topic_id: Some(id),
        with_hidden: staff,
        with_deleted_topics: true,
        ..PostFilter::default()
    };
    let found = forum::posts(db, &filter, (number - 1) * POSTS_PAGE, POSTS_PAGE + 1).await?;
    let more = found.len() > POSTS_PAGE as usize;
    let found = &found[..found.len().min(POSTS_PAGE as usize)];
    let me = page.current.user.as_ref().map(|u| u.id);
    let votes = match me {
        Some(me) => {
            forum::votes_of(db, me, &found.iter().map(|p| p.id).collect::<Vec<_>>()).await?
        }
        None => Vec::new(),
    };
    if let Some(me) = me {
        forum::visit(db, me, id).await?;
    }
    let can_post = me.is_some() && page.current.can(Permission::Comment);
    let posts: Vec<Value> = found
        .iter()
        .map(|p| {
            let mine = me.is_some() && p.creator_id == me;
            context! {
                id => p.id,
                creator => p.creator_name,
                updater => p.updater_name.as_ref().filter(|_| p.updated_at > p.created_at),
                html => Value::from_safe_string(markup::render(&p.body)),
                hidden => p.is_hidden,
                score => p.score,
                vote => votes.iter().find(|(post, _)| *post == p.id).map(|(_, s)| *s),
                date => crate::dates::day(p.created_at),
                time => crate::dates::clock(p.created_at),
                can_edit => (mine && !topic.is_locked) || staff,
                can_vote => can_post && page.current.can(Permission::Vote) && !mine,
                quote => markup::quote(p.creator_name.as_deref().unwrap_or("someone"), &p.body),
            }
        })
        .collect();
    let (relation, request) = forum::topic_request(db, id).await?;
    let request_url = match (relation, request) {
        (Some(r), _) => moekura_db::tag_relations::by_id(db, r)
            .await?
            .map(|r| crate::requests::relation_url(r.kind, r.id)),
        (None, Some(r)) => Some(format!("/tags/requests/{r}")),
        (None, None) => None,
    };
    let page_url = |n: i64| url_value(&format!("{}?page={n}", topic_url(id)));
    Ok(page.render(
        "forum_topic.html",
        context! {
            topic => topic_context(&topic),
            posts => posts,
            merged_into => topic.merged_into_id,
            request_url => request_url.map(|u| url_value(&u)),
            can_reply => can_post && !topic.is_deleted && (!topic.is_locked || staff),
            can_moderate => staff,
            can_edit_topic => staff || (me.is_some() && topic.creator_id == me && !topic.is_locked),
            categories => forum::categories(db).await?.iter().map(|c| context! { id => c.id, name => c.name }).collect::<Vec<_>>(),
            previous_url => (number > 1).then(|| page_url(number - 1)),
            next_url => more.then(|| page_url(number + 1)),
        },
    ))
}

async fn update_topic(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<TopicForm>,
) -> Result<Response, AppError> {
    let me = require_poster(&page.current)?;
    let topic = visible_topic(page.state(), &page.current, id).await?;
    let staff = moderates(&page.current);
    if !(staff || (topic.creator_id == Some(me) && !topic.is_locked)) {
        return Err(AppError::Forbidden);
    }
    let title = clean_title(&form.title)?;
    let db = page.state().db.primary();
    if !forum::categories(db)
        .await?
        .iter()
        .any(|c| c.id == form.category)
    {
        return Err(AppError::Unprocessable("Choose a category.".into()));
    }
    forum::update_topic(db, id, &title, form.category).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&topic_url(id))).into_response())
}

#[derive(Debug, Deserialize)]
struct ModerateForm {
    /// `sticky`, `unsticky`, `lock`, `unlock`, `delete` or `undelete`.
    action: String,
}

async fn moderate(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<ModerateForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ModerateComments)?;
    visible_topic(page.state(), &page.current, id).await?;
    let flags = match form.action.as_str() {
        "sticky" => Flags {
            sticky: Some(true),
            ..Flags::default()
        },
        "unsticky" => Flags {
            sticky: Some(false),
            ..Flags::default()
        },
        "lock" => Flags {
            locked: Some(true),
            ..Flags::default()
        },
        "unlock" => Flags {
            locked: Some(false),
            ..Flags::default()
        },
        "delete" => Flags {
            deleted: Some(true),
            ..Flags::default()
        },
        "undelete" => Flags {
            deleted: Some(false),
            ..Flags::default()
        },
        _ => return Err(AppError::BadRequest("Unknown action.".into())),
    };
    let db = page.state().db.primary();
    forum::set_flags(db, id, flags).await?;
    mod_actions::record(
        db,
        NewAction::new(
            page.current.user.as_ref().map(|u| u.id),
            ActionKind::ForumTopicModerate,
        )
        .details(serde_json::json!({ "topic_id": id, "action": form.action })),
    )
    .await?;
    let back = if form.action == "delete" {
        "/forum_topics".to_owned()
    } else {
        topic_url(id)
    };
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&back)).into_response())
}

#[derive(Debug, Deserialize)]
struct MergeForm {
    /// The topic to move the posts into, by number.
    #[serde(default)]
    into: String,
}

async fn merge(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<MergeForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ModerateComments)?;
    let state = page.state();
    visible_topic(state, &page.current, id).await?;
    let into: i64 = form
        .into
        .trim()
        .trim_start_matches('#')
        .parse()
        .map_err(|_| {
            AppError::Unprocessable("Give the number of the topic to merge into.".into())
        })?;
    let target = visible_topic(state, &page.current, into).await?;
    if target.id == id || target.is_deleted {
        return Err(AppError::Unprocessable(
            "Merge into another topic that isn't deleted.".into(),
        ));
    }
    let db = state.db.primary();
    forum::merge(db, id, into).await?;
    mod_actions::record(
        db,
        NewAction::new(
            page.current.user.as_ref().map(|u| u.id),
            ActionKind::ForumTopicMerge,
        )
        .details(serde_json::json!({ "from": id, "into": into })),
    )
    .await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&topic_url(into)),
    )
        .into_response())
}

async fn mark_all_read(page: Page, jar: CookieJar) -> Result<Response, AppError> {
    let me = page
        .current
        .user
        .as_ref()
        .map(|u| u.id)
        .ok_or(AppError::Unauthorized)?;
    forum::mark_all_read(page.state().db.primary(), me).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/forum_topics")).into_response())
}

// ---- posts ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PostForm {
    #[serde(default)]
    body: String,
}

/// Where post `id` is: its topic's page.
async fn post_location(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<String, AppError> {
    let db = state.db.primary();
    let p = forum::post(db, id).await?.ok_or(AppError::NotFound)?;
    let at = forum::position(db, p.topic_id, id, moderates(current)).await?;
    Ok(format!(
        "{}?page={}#forum-post-{id}",
        topic_url(p.topic_id),
        at / POSTS_PAGE + 1
    ))
}

async fn reply(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<PostForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    let post = add_post(state, &page.current, id, &form.body).await?;
    if post.held {
        return Ok((flash::set(jar, Flash::Held), Redirect::to(&topic_url(id))).into_response());
    }
    let location = post_location(state, &page.current, post.id).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&location)).into_response())
}

async fn goto_post(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let state = page.state();
    let p = forum::post(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    visible_topic(state, &page.current, p.topic_id).await?;
    if p.is_hidden && !moderates(&page.current) {
        return Err(AppError::NotFound);
    }
    Ok(Redirect::to(&post_location(state, &page.current, id).await?).into_response())
}

/// Post `id`, if `current` may change it: their own in an open topic, or
/// anyone's for staff.
async fn editable_post(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
) -> Result<forum::Post, AppError> {
    let me = require_poster(current)?;
    let p = forum::post(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    let topic = visible_topic(state, current, p.topic_id).await?;
    if !(moderates(current) || (p.creator_id == Some(me) && !topic.is_locked && !p.is_hidden)) {
        return Err(AppError::Forbidden);
    }
    Ok(p)
}

async fn edit_form(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    let p = editable_post(page.state(), &page.current, id).await?;
    Ok(page.render(
        "forum_post_edit.html",
        context! { id => p.id, topic => p.topic_title, topic_id => p.topic_id, body => p.body },
    ))
}

async fn edit_post(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<PostForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    let p = editable_post(state, &page.current, id).await?;
    let body = clean_body(&form.body)?;
    if save_edit(state, &page.current, &p, &body).await? {
        return Ok((
            flash::set(jar, Flash::Held),
            Redirect::to(&topic_url(p.topic_id)),
        )
            .into_response());
    }
    let location = post_location(state, &page.current, id).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&location)).into_response())
}

/// Saves `body` (already cleaned) as the new text of post `p`, which
/// [`editable_post`] let `current` change. Like a new post, a changed text
/// counts against the rate limit and goes through the spam filter, which
/// can hold the post again; returns whether it did.
async fn save_edit(
    state: &AppState,
    current: &CurrentUser,
    p: &forum::Post,
    body: &str,
) -> Result<bool, AppError> {
    let me = require_poster(current)?;
    let held = if body == p.body {
        None
    } else {
        state.rate_limits.check_comment(me).await?;
        crate::held::check(state, current, body).await?
    };
    forum::update_post(state.db.primary(), p.id, body, Some(me), held.as_deref()).await?;
    if held.is_some() {
        tracing::info!(forum_post = p.id, "forum post edit held");
    }
    Ok(held.is_some())
}

#[derive(Debug, Deserialize)]
struct VoteForm {
    /// `1`, `-1`, or `0` to take a vote back.
    score: i16,
}

async fn vote(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<VoteForm>,
) -> Result<Response, AppError> {
    let me = require_poster(&page.current)?;
    page.current.require(Permission::Vote)?;
    let state = page.state();
    let p = forum::post(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)?;
    visible_topic(state, &page.current, p.topic_id).await?;
    if p.creator_id == Some(me) {
        return Err(AppError::Unprocessable(
            "You can't vote on your own post.".into(),
        ));
    }
    forum::vote(state.db.primary(), id, me, form.score.clamp(-1, 1)).await?;
    let location = post_location(state, &page.current, id).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&location)).into_response())
}

async fn set_hidden(
    page: Page,
    jar: CookieJar,
    id: i64,
    hidden: bool,
) -> Result<Response, AppError> {
    page.current.require(Permission::ModerateComments)?;
    let state = page.state();
    let db = state.db.primary();
    let p = forum::post(db, id).await?.ok_or(AppError::NotFound)?;
    forum::set_post_hidden(db, id, hidden).await?;
    let kind = if hidden {
        ActionKind::ForumPostHide
    } else {
        ActionKind::ForumPostUnhide
    };
    let mut action = NewAction::new(page.current.user.as_ref().map(|u| u.id), kind)
        .details(serde_json::json!({ "forum_post_id": id, "topic_id": p.topic_id }));
    if let Some(creator) = p.creator_id {
        action = action.user(creator);
    }
    mod_actions::record(db, action).await?;
    let location = post_location(state, &page.current, id).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&location)).into_response())
}

async fn hide(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    set_hidden(page, jar, id, true).await
}

async fn unhide(page: Page, jar: CookieJar, Path(id): Path<i64>) -> Result<Response, AppError> {
    set_hidden(page, jar, id, false).await
}

#[derive(Debug, Default, Deserialize)]
struct SearchQuery {
    #[serde(default)]
    body: String,
    #[serde(default)]
    user: String,
    page: Option<i64>,
}

/// Searching posts by their words or writer.
async fn search(page: Page, Query(query): Query<SearchQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let number = query.page.unwrap_or(1).clamp(1, 1000);
    let creator_id = match query.user.trim() {
        "" => None,
        name => Some(
            moekura_db::users::by_name(db, name)
                .await?
                .map_or(-1, |u| u.id),
        ),
    };
    let searched = !query.body.trim().is_empty() || creator_id.is_some();
    let found = if searched {
        let filter = PostFilter {
            creator_id,
            words: &query.body,
            ..PostFilter::default()
        };
        forum::posts(db, &filter, (number - 1) * POSTS_PAGE, POSTS_PAGE + 1).await?
    } else {
        Vec::new()
    };
    let more = found.len() > POSTS_PAGE as usize;
    let list_url = |n: i64| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("body", &query.body)
            .append_pair("user", &query.user)
            .append_pair("page", &n.to_string())
            .finish();
        url_value(&format!("/forum_posts?{q}"))
    };
    Ok(page.render(
        "forum_posts.html",
        context! {
            query => context! { body => query.body, user => query.user },
            searched => searched,
            posts => found.iter().take(POSTS_PAGE as usize).map(|p| context! {
                id => p.id,
                topic => p.topic_title,
                creator => p.creator_name,
                html => Value::from_safe_string(markup::render(&markup::excerpt(&p.body))),
                date => crate::dates::day(p.created_at),
            }).collect::<Vec<_>>(),
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => more.then(|| list_url(number + 1)),
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    fn id_after(location: &str, prefix: &str) -> i64 {
        location[prefix.len()..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn topics_posts_and_moderation(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let staff = session_for(&pool, "staff", SystemRole::Moderator).await;
        let general = moekura_db::forum::categories(&pool).await.unwrap()[0].id;

        let created = app
            .post_form(
                "/forum_topics",
                Some(&alice),
                &[],
                &format!("category={general}&title=Hello+there&body=First+%5B%5Bcat%5D%5D"),
            )
            .await;
        assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
        let topic = id_after(&created.location.unwrap(), "/forum_topics/");
        let list = app.get("/forum_topics", Some(&bob)).await.body;
        assert!(
            list.contains("Hello there") && list.contains("unread"),
            "{list}"
        );

        let replied = app
            .post_form(
                &format!("/forum_topics/{topic}/posts"),
                Some(&bob),
                &[],
                "body=A+reply",
            )
            .await;
        assert_eq!(replied.status, StatusCode::SEE_OTHER, "{}", replied.body);
        let location = replied.location.unwrap();
        let post = id_after(location.rsplit("#forum-post-").next().unwrap(), "");
        let shown = app
            .get(&format!("/forum_topics/{topic}"), Some(&alice))
            .await
            .body;
        assert!(
            shown.contains("href=\"/wiki/cat\"") && shown.contains("A reply"),
            "{shown}"
        );

        let voted = app
            .post_form(
                &format!("/forum_posts/{post}/vote"),
                Some(&alice),
                &[],
                "score=1",
            )
            .await;
        assert_eq!(voted.status, StatusCode::SEE_OTHER);
        let own = app
            .post_form(
                &format!("/forum_posts/{post}/vote"),
                Some(&bob),
                &[],
                "score=1",
            )
            .await;
        assert_eq!(own.status, StatusCode::UNPROCESSABLE_ENTITY);
        // Others can't edit it.
        assert_eq!(
            app.post_form(&format!("/forum_posts/{post}"), Some(&alice), &[], "body=x")
                .await
                .status,
            StatusCode::FORBIDDEN
        );

        // Locked: only staff reply.
        app.post_form(
            &format!("/forum_topics/{topic}/moderate"),
            Some(&staff),
            &[],
            "action=lock",
        )
        .await;
        let refused = app
            .post_form(
                &format!("/forum_topics/{topic}/posts"),
                Some(&bob),
                &[],
                "body=Again",
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            app.post_form(
                &format!("/forum_topics/{topic}/moderate"),
                Some(&bob),
                &[],
                "action=unlock"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );

        // Hidden posts are gone for others, there for staff.
        app.post(&format!("/forum_posts/{post}/hide"), Some(&staff), &[])
            .await;
        assert!(
            !app.get(&format!("/forum_topics/{topic}"), Some(&alice))
                .await
                .body
                .contains("A reply")
        );
        assert!(
            app.get(&format!("/forum_topics/{topic}"), Some(&staff))
                .await
                .body
                .contains("A reply")
        );

        // Merging moves the posts and sends readers on.
        let other = app
            .post_form(
                "/forum_topics",
                Some(&bob),
                &[],
                &format!("category={general}&title=Duplicate&body=Same+thing"),
            )
            .await;
        let other = id_after(&other.location.unwrap(), "/forum_topics/");
        let merged = app
            .post_form(
                &format!("/forum_topics/{other}/merge"),
                Some(&staff),
                &[],
                &format!("into={topic}"),
            )
            .await;
        assert_eq!(merged.status, StatusCode::SEE_OTHER, "{}", merged.body);
        assert_eq!(
            app.get(&format!("/forum_topics/{other}"), Some(&alice))
                .await
                .location
                .as_deref(),
            Some(format!("/forum_topics/{topic}").as_str())
        );
        let found = app.get("/forum_posts?body=same", None).await.body;
        assert!(found.contains("Same thing"), "{found}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn edits_go_through_the_spam_filter(pool: PgPool) {
        moekura_db::settings::set(
            &pool,
            "spam_filter",
            serde_json::json!({ "words": ["casino"] }),
        )
        .await
        .unwrap();
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let staff = session_for(&pool, "staff", SystemRole::Moderator).await;
        let general = moekura_db::forum::categories(&pool).await.unwrap()[0].id;
        let created = app
            .post_form(
                "/forum_topics",
                Some(&alice),
                &[],
                &format!("category={general}&title=Hello&body=Harmless"),
            )
            .await;
        let topic = id_after(&created.location.unwrap(), "/forum_topics/");
        let post: i64 = sqlx::query_scalar("SELECT id FROM forum_posts WHERE topic_id = $1")
            .bind(topic)
            .fetch_one(&pool)
            .await
            .unwrap();

        let edited = app
            .post_form(
                &format!("/forum_posts/{post}"),
                Some(&alice),
                &[],
                "body=Best+casino+deals",
            )
            .await;
        assert_eq!(edited.status, StatusCode::SEE_OTHER, "{}", edited.body);
        assert_eq!(
            edited.location.as_deref(),
            Some(&*format!("/forum_topics/{topic}"))
        );
        let shown = app
            .get(&format!("/forum_topics/{topic}"), Some(&alice))
            .await
            .body;
        assert!(!shown.contains("casino"), "{shown}");
        let held: Option<String> =
            sqlx::query_scalar("SELECT held_reason FROM forum_posts WHERE id = $1")
                .bind(post)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(held.as_deref(), Some("contains “casino”"));

        // Staff edits are never held.
        app.post_form(
            &format!("/forum_posts/{post}"),
            Some(&staff),
            &[],
            "body=More+casino",
        )
        .await;
        let body: String = sqlx::query_scalar("SELECT body FROM forum_posts WHERE id = $1")
            .bind(post)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(body, "More casino");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn tag_requests_get_topics(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes()
                .merge(crate::tag_relations::routes())
                .merge(crate::requests::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let requested = app
            .post_form(
                "/tags/aliases",
                Some(&alice),
                &[],
                "antecedent=kitty&consequent=cat&reason=Same+animal",
            )
            .await;
        assert_eq!(
            requested.status,
            StatusCode::SEE_OTHER,
            "{}",
            requested.body
        );
        let topics = moekura_db::forum::topics(&pool, None, &Default::default(), 0, 10)
            .await
            .unwrap();
        assert_eq!(topics.len(), 1);
        assert_eq!(topics[0].title, "Alias request: kitty → cat");
        assert_eq!(topics[0].category_name, "Tags");
        let shown = app
            .get(&format!("/forum_topics/{}", topics[0].id), None)
            .await
            .body;
        assert!(
            shown.contains("Same animal") && shown.contains("/tags/aliases/"),
            "{shown}"
        );
        let relation = moekura_db::forum::topic_request(&pool, topics[0].id)
            .await
            .unwrap()
            .0
            .unwrap();
        let request = app
            .get(&format!("/tags/aliases/{relation}"), None)
            .await
            .body;
        assert!(
            request.contains(&format!("/forum_topics/{}", topics[0].id)),
            "{request}"
        );
    }
}
