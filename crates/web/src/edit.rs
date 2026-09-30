//! Editing posts: tags, rating, source, description and parent.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use moekura_core::permissions::Permission;
use moekura_core::post_edit::Metatag;
use moekura_core::posts::{DESCRIPTION_MAX_LEN, PostLock, Rating, SOURCE_MAX_LEN};
use moekura_core::tags::POST_MAX_TAGS;
use moekura_db::posts::{self, PostEdit};
use moekura_db::tags::{self, WantedTag};
use serde::Deserialize;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::{Extra, FailedEdit, render_post, visibility};
use crate::tags::{TagFieldError, parse_edit, too_many};

pub fn routes() -> Router<AppState> {
    Router::new().route("/posts/{id}/edit", post(edit))
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct EditForm {
    #[serde(default)]
    pub tags: String,
    /// The tags the form was shown with, to tell what this edit changed.
    #[serde(default)]
    pub old_tags: String,
    #[serde(default)]
    pub rating: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parent: String,
    /// A suggested tag clicked on (without scripts, which add it to
    /// `tags` instead).
    #[serde(default)]
    pub add: String,
    /// The suggested rating, clicked on (without scripts, which pick it
    /// among `rating` instead).
    #[serde(default)]
    pub suggested_rating: String,
    /// A related post whose tags to add, clicked on (without scripts,
    /// which add them to `tags` instead).
    #[serde(default)]
    pub copy_from: String,
}

#[derive(Debug, Default, Deserialize)]
struct EditQuery {
    /// The search the post was opened from, kept across the edit.
    #[serde(default)]
    q: String,
}

/// Why an edit was refused, for the form.
pub(crate) enum Refused {
    Invalid(String),
    Error(AppError),
}

impl From<sqlx::Error> for Refused {
    fn from(error: sqlx::Error) -> Self {
        Refused::Error(error.into())
    }
}

impl From<TagFieldError> for Refused {
    fn from(error: TagFieldError) -> Self {
        match error {
            TagFieldError::Invalid(message) => Refused::Invalid(message),
            TagFieldError::Db(error) => error.into(),
        }
    }
}

async fn edit(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Query(query): Query<EditQuery>,
    Form(form): Form<EditForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditPosts)?;
    match apply(page.state(), &page.current, id, &form).await {
        Ok(()) => {
            let kept =
                crate::tag_warnings::kept_categories(page.state().db.primary(), &form.tags, id)
                    .await?;
            let mut back = format!("/posts/{id}?{}", crate::tag_warnings::check_query(&kept));
            if !query.q.is_empty() {
                let q = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("q", &query.q)
                    .finish();
                back = format!("{back}&{q}");
            }
            Ok((flash::set(jar, Flash::Saved), Redirect::to(&back)).into_response())
        }
        Err(Refused::Invalid(error)) => {
            render_post(
                &page,
                id,
                (!query.q.is_empty()).then_some(query.q.as_str()),
                true,
                Extra {
                    failed_edit: Some(FailedEdit { form: &form, error }),
                    ..Extra::default()
                },
            )
            .await
        }
        Err(Refused::Error(error)) => Err(error),
    }
}

/// Validates `form` and saves it in one transaction with the post locked;
/// then applies the tag box's metatags for other things (pools,
/// favorites, …).
pub(crate) async fn apply(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    form: &EditForm,
) -> Result<(), Refused> {
    let invalid = |message: &str| Err(Refused::Invalid(message.to_owned()));
    let db = state.db.primary();
    let mut tags = std::borrow::Cow::Borrowed(&form.tags);
    if !form.add.trim().is_empty() {
        tags = std::borrow::Cow::Owned(format!("{tags} {}", form.add));
    }
    if !form.copy_from.trim().is_empty() {
        let copied = copied_tags(state, current, &form.copy_from).await?;
        tags = std::borrow::Cow::Owned(format!("{tags} {copied}"));
    }
    let changes = parse_edit(db, &form.old_tags, &tags).await?;
    let rating = if form.suggested_rating.is_empty() {
        &form.rating
    } else {
        &form.suggested_rating
    };
    let Ok(mut rating) = rating.parse::<Rating>() else {
        return invalid("Choose a rating.");
    };
    let mut source = form.source.trim().to_owned();
    let mut parent = form.parent.trim().trim_start_matches('#').to_owned();
    // Metatags for the post's own fields win over the form's.
    for metatag in &changes.added.metatags {
        match metatag {
            Metatag::Rating(r) => rating = *r,
            Metatag::Source(s) => source.clone_from(s),
            Metatag::Parent(p) => parent = p.map(|p| p.to_string()).unwrap_or_default(),
            Metatag::RemoveParent(p) if parent == p.to_string() => parent.clear(),
            _ => {}
        }
    }
    let effects =
        crate::metatags::prepare(state, current, Some(id), &changes.added.metatags).await?;
    let source = source.as_str();
    let description = form.description.trim();
    if source.chars().count() > SOURCE_MAX_LEN {
        return invalid(&format!(
            "The source may be at most {SOURCE_MAX_LEN} characters."
        ));
    }
    if description.chars().count() > DESCRIPTION_MAX_LEN {
        return invalid(&format!(
            "The description may be at most {DESCRIPTION_MAX_LEN} characters."
        ));
    }
    let parent_id = match parent.as_str() {
        "" => None,
        text => match text.parse::<i64>() {
            Ok(parent) if parent == id => return invalid("A post can't be its own parent."),
            Ok(parent) => Some(parent),
            Err(_) => return invalid("The parent must be a post number."),
        },
    };

    let mut tx = db.begin().await?;
    let post = posts::lock(&mut *tx, id)
        .await?
        .ok_or(Refused::Error(AppError::NotFound))?;
    if !visibility(current).allows(&post) {
        return Err(Refused::Error(AppError::NotFound));
    }
    if let Some(parent) = parent_id
        && parent_id != post.parent_id
    {
        let exists = posts::by_id(&mut *tx, parent).await?.is_some();
        if !exists {
            return invalid(&format!("There is no post #{parent}."));
        }
        if posts::has_ancestor(&mut *tx, parent, id).await? {
            return invalid(&format!(
                "Post #{parent} descends from this post, so it can't be its parent."
            ));
        }
    }

    // Apply this form's changes to the tags as they are now, so an edit
    // made by someone else meanwhile isn't undone.
    let kept: Vec<String> = tags::by_ids(&mut *tx, &post.tag_ids)
        .await?
        .into_iter()
        .map(|t| t.name)
        .filter(|name| !changes.removed.contains(name))
        .collect();
    let mut wanted: Vec<WantedTag<'_>> = kept
        .iter()
        .map(|name| WantedTag {
            name,
            category_id: None,
        })
        .collect();
    for added in changes.added.wanted() {
        if !wanted.iter().any(|w| w.name == added.name) {
            wanted.push(added);
        }
    }
    if wanted.len() > POST_MAX_TAGS {
        return Err(too_many().into());
    }
    // Before the tags, so tags this creates or recategorises are credited.
    moekura_db::post_versions::attribute(&mut tx, current.user.as_ref().map(|u| u.id), None)
        .await?;
    let found = tags::for_post(&mut tx, &wanted, current.can(Permission::ManageTags)).await?;
    let request_tags = state.site.get().settings.request_tags;
    let tag_ids: Vec<i32> = crate::tag_warnings::with_request_tags(&mut tx, request_tags, found)
        .await?
        .iter()
        .map(|t| t.id)
        .collect();

    for (changed, lock) in [
        (tag_ids != post.tag_ids, PostLock::Tags),
        (rating != post.rating, PostLock::Rating),
    ] {
        if changed && crate::posts::check_lock(current, &post, lock).is_err() {
            return Err(Refused::Invalid(crate::posts::locked_message(lock)));
        }
    }
    posts::update(
        &mut *tx,
        id,
        PostEdit {
            rating,
            source,
            description,
            parent_id,
            tag_ids: &tag_ids,
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(
        post_id = id,
        user = current.user.as_ref().map(|u| u.name.as_str()),
        "post edited"
    );
    crate::metatags::apply(state, current, id, &effects).await
}

/// The tags of post `id` (as typed in a form), if `current` may see it.
async fn copied_tags(state: &AppState, current: &CurrentUser, id: &str) -> Result<String, Refused> {
    let db = state.db.primary();
    let missing = || Refused::Invalid(format!("There is no post #{id}."));
    let id: i64 = id
        .trim()
        .trim_start_matches('#')
        .parse()
        .map_err(|_| missing())?;
    let post = posts::by_id(db, id)
        .await?
        .filter(|p| visibility(current).allows(p))
        .ok_or_else(missing)?;
    // A tag spelled like a metatag (from before metatags) isn't copied,
    // since the box would read it as one.
    Ok(tags::by_ids(db, &post.tag_ids)
        .await?
        .into_iter()
        .map(|t| t.name)
        .filter(|name| {
            moekura_core::post_edit::parse(name, &[], &[])
                .metatags
                .is_empty()
        })
        .collect::<Vec<_>>()
        .join(" "))
}

/// Sets post `id`'s parent as `current` (for `child:` metatags), with the
/// same checks as the edit form, recorded in its history.
pub(crate) async fn set_parent(
    state: &AppState,
    current: &CurrentUser,
    id: i64,
    parent_id: Option<i64>,
) -> Result<(), Refused> {
    let invalid = |message: String| Err(Refused::Invalid(message));
    let db = state.db.primary();
    let mut tx = db.begin().await?;
    let Some(post) = posts::lock(&mut *tx, id).await? else {
        return invalid(format!("There is no post #{id}."));
    };
    if !visibility(current).allows(&post) {
        return invalid(format!("There is no post #{id}."));
    }
    if post.parent_id == parent_id {
        return Ok(());
    }
    if let Some(parent) = parent_id {
        if parent == id {
            return invalid("A post can't be its own parent.".to_owned());
        }
        if posts::by_id(&mut *tx, parent).await?.is_none() {
            return invalid(format!("There is no post #{parent}."));
        }
        if posts::has_ancestor(&mut *tx, parent, id).await? {
            return invalid(format!(
                "Post #{parent} descends from post #{id}, so it can't be its parent."
            ));
        }
    }
    moekura_db::post_versions::attribute(&mut tx, current.user.as_ref().map(|u| u.id), None)
        .await?;
    posts::update(
        &mut *tx,
        id,
        PostEdit {
            rating: post.rating,
            source: &post.source,
            description: &post.description,
            parent_id,
            tag_ids: &post.tag_ids,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        let state = test_state(pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let routes = super::routes()
            .merge(crate::posts::routes())
            .merge(crate::upload::routes(max));
        TestApp::new(state, routes)
    }

    async fn upload(app: &TestApp, session: &str, png: &[u8], tags: &str) -> i64 {
        let fields = vec![("rating", "s".to_owned()), ("tags", tags.to_owned())];
        let response = app
            .post_multipart("/upload", Some(session), &fields, Some(("a.png", png)))
            .await;
        response.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    fn form(pairs: &[(&str, &str)]) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish()
    }

    async fn tag_names(pool: &PgPool, id: i64) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT t.name FROM posts p JOIN tags t ON t.id = ANY(p.tag_ids)
             WHERE p.id = $1 ORDER BY t.name",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn edits_merge_with_concurrent_ones(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let id = upload(&app, &alice, &fixture::png(20, 20), "cat cute").await;
        let path = format!("/posts/{id}/edit");

        // Two people open the form; the first adds `dog`, the second
        // removes `cute` and sets other fields.
        let first = form(&[
            ("old_tags", "cat cute"),
            ("tags", "cat cute dog"),
            ("rating", "s"),
        ]);
        let response = app.post_form(&path, Some(&alice), &[], &first).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let second = form(&[
            ("old_tags", "cat cute"),
            ("tags", "cat artist:someone"),
            ("rating", "e"),
            ("source", " https://example.com/a "),
            ("description", "edited"),
        ]);
        let response = app.post_form(&path, Some(&alice), &[], &second).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);

        assert_eq!(tag_names(&pool, id).await, ["cat", "dog", "someone"]);
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.rating.code(), "e");
        assert_eq!(post.source, "https://example.com/a");
        assert_eq!(post.description, "edited");
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await;
        assert!(
            page.body.contains("value=\"cat dog someone\""),
            "{}",
            page.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn metatags_in_the_tag_box(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let visitor_like = session_for(&pool, "bob", SystemRole::Member).await;
        let id = upload(&app, &alice, &fixture::png(20, 20), "cat cute").await;
        let other = upload(&app, &alice, &fixture::png(24, 20), "dog").await;
        let path = format!("/posts/{id}/edit");
        let alice_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let group = moekura_db::favorite_groups::create(
            &pool,
            alice_id,
            &moekura_db::favorite_groups::Contents {
                name: "mine".into(),
                is_public: false,
                post_ids: Vec::new(),
            },
        )
        .await
        .unwrap();

        let edit = form(&[
            ("old_tags", "cat cute"),
            (
                "tags",
                &format!(
                    "cat cute -cute rating:e source:https://example.com/x child:{other} \
                     newpool:My_Comic fav favgroup:mine upvote"
                ),
            ),
            ("rating", "s"),
        ]);
        let response = app.post_form(&path, Some(&alice), &[], &edit).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        assert_eq!(tag_names(&pool, id).await, ["cat"]);
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (post.rating.code(), post.source.as_str()),
            ("e", "https://example.com/x")
        );
        assert_eq!((post.fav_count, post.score), (1, 1));
        let child = moekura_db::posts::by_id(&pool, other)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.parent_id, Some(id));
        let history = moekura_db::post_versions::list(&pool, other).await.unwrap();
        assert_eq!(history[0].updater_name.as_deref(), Some("alice"));
        let comic = moekura_db::pools::by_name(&pool, "my_comic")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            moekura_db::pools::post_ids(&pool, comic.id).await.unwrap(),
            [id]
        );
        assert_eq!(
            moekura_db::favorite_groups::post_ids(&pool, group)
                .await
                .unwrap(),
            [id]
        );

        // And undone.
        let undo = form(&[
            ("old_tags", "cat"),
            (
                "tags",
                &format!(
                    "cat -child:{other} -pool:{} -fav -favgroup:{group} -parent parent:none",
                    comic.id
                ),
            ),
            ("rating", "e"),
        ]);
        let response = app.post_form(&path, Some(&alice), &[], &undo).await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let child = moekura_db::posts::by_id(&pool, other)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.parent_id, None);
        assert!(
            moekura_db::pools::post_ids(&pool, comic.id)
                .await
                .unwrap()
                .is_empty()
        );
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.fav_count, 0);

        // Mistakes and what someone can't do are refused before saving.
        for (tags, error) in [
            ("cat rating:x", "isn&#x27;t a rating"),
            ("cat pool:nope", "no pool called"),
            ("cat favgroup:mine", "no favorite group"),
            ("cat child:999", "no post #999"),
        ] {
            let bad = form(&[("old_tags", "cat"), ("tags", tags), ("rating", "e")]);
            let response = app.post_form(&path, Some(&visitor_like), &[], &bad).await;
            assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY, "{tags}");
            assert!(response.body.contains(error), "{tags}: {}", response.body);
        }
        // A tag spelled like a metatag, from before, stays a tag.
        sqlx::query("INSERT INTO tags (name) VALUES ('fav')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE posts SET tag_ids = uniq(sort(tag_ids || (SELECT id FROM tags WHERE name = 'fav')))
             WHERE id = $1",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        let kept = form(&[
            ("old_tags", "cat fav"),
            ("tags", "cat fav"),
            ("rating", "e"),
        ]);
        app.post_form(&path, Some(&alice), &[], &kept).await;
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(post.fav_count, 0);
        assert_eq!(tag_names(&pool, id).await, ["cat", "fav"]);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn copies_tags_from_the_parent_and_children(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let parent = upload(&app, &alice, &fixture::png(20, 20), "cat long_hair").await;
        let id = upload(
            &app,
            &alice,
            &fixture::png(24, 20),
            &format!("solo parent:{parent}"),
        )
        .await;
        let child = upload(
            &app,
            &alice,
            &fixture::png(28, 20),
            &format!("dog parent:{id}"),
        )
        .await;

        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await.body;
        assert!(
            page.contains(&format!("name=\"copy_from\" value=\"{parent}\""))
                && page.contains("data-tags=\"cat long_hair\"")
                && page.contains(&format!("From child #{child}")),
            "{page}"
        );
        // Without scripts, a click adds them and saves.
        let copy = form(&[
            ("old_tags", "solo"),
            ("tags", "solo"),
            ("rating", "s"),
            ("parent", &parent.to_string()),
            ("copy_from", &parent.to_string()),
        ]);
        let response = app
            .post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], &copy)
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        assert_eq!(tag_names(&pool, id).await, ["cat", "long_hair", "solo"]);
        let bad = form(&[
            ("old_tags", "solo"),
            ("tags", "solo"),
            ("rating", "s"),
            ("copy_from", "99999"),
        ]);
        let response = app
            .post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], &bad)
            .await;
        assert!(
            response.body.contains("no post #99999"),
            "{}",
            response.body
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn metatags_on_upload(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let parent = upload(&app, &alice, &fixture::png(20, 20), "cat").await;
        let fields = vec![
            (
                "tags",
                format!("dog -cat rating:q parent:{parent} newpool:Series fav"),
            ),
            ("source", "https://example.com/field".to_owned()),
        ];
        let response = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &fields,
                Some(("b.png", &fixture::png(24, 20))),
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let id: i64 = response.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(tag_names(&pool, id).await, ["dog"]);
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!(
            (post.rating.code(), post.parent_id, post.fav_count),
            ("q", Some(parent), 1)
        );
        let series = moekura_db::pools::by_name(&pool, "series")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            moekura_db::pools::post_ids(&pool, series.id).await.unwrap(),
            [id]
        );
        // A bad metatag stops the upload.
        let fields = vec![
            ("rating", "s".to_owned()),
            ("tags", "dog pool:missing".to_owned()),
        ];
        let refused = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &fields,
                Some(("c.png", &fixture::png(28, 20))),
            )
            .await;
        assert!(refused.body.contains("no pool called"), "{}", refused.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn parents_and_refusals(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let parent = upload(&app, &alice, &fixture::png(20, 20), "a").await;
        let child = upload(&app, &alice, &fixture::png(24, 20), "b").await;
        let edit = |parent: &str, tags: &str| {
            form(&[
                ("old_tags", ""),
                ("tags", tags),
                ("rating", "g"),
                ("parent", parent),
            ])
        };

        let response = app
            .post_form(
                &format!("/posts/{child}/edit?q=b"),
                Some(&alice),
                &[],
                &edit(&format!("#{parent}"), "b"),
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        assert_eq!(
            response.location.as_deref(),
            Some(format!("/posts/{child}?check=1&q=b").as_str())
        );
        // Both posts show the family bar.
        for id in [parent, child] {
            let page = app.get(&format!("/posts/{id}"), None).await;
            assert!(page.body.contains("class=\"family\""), "{}", page.body);
            assert!(page.body.contains(&format!("href=\"/posts/{child}\"")));
        }

        let cases = [
            (parent, child.to_string(), "b", "descends from this post"),
            (parent, parent.to_string(), "b", "its own parent"),
            (parent, "99999".to_owned(), "b", "There is no post #99999"),
            (parent, "x".to_owned(), "b", "must be a post number"),
            (parent, String::new(), "--bad", "may not start with `-`"),
        ];
        for (id, parent_field, tags, message) in cases {
            let response = app
                .post_form(
                    &format!("/posts/{id}/edit"),
                    Some(&alice),
                    &[],
                    &edit(&parent_field, tags),
                )
                .await;
            assert_eq!(
                response.status,
                StatusCode::UNPROCESSABLE_ENTITY,
                "{message}"
            );
            assert!(
                response.body.contains(message),
                "{message}: {}",
                response.body
            );
            // The form keeps what was typed.
            assert!(response.body.contains(&format!("value=\"{parent_field}\"")));
        }

        let anonymous = app
            .post_form(&format!("/posts/{child}/edit"), None, &[], &edit("", "b"))
            .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn locked_posts_hold_their_rating_tags_notes_and_status(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            super::routes()
                .merge(crate::posts::routes())
                .merge(crate::history::routes())
                .merge(crate::moderation::routes())
                .merge(crate::notes::routes())
                .merge(crate::upload::routes(max)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let janitor = session_for(&pool, "jan", SystemRole::Janitor).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let id = upload(&app, &alice, &fixture::png(20, 20), "cat").await;
        let lock = format!("/posts/{id}/locks");
        assert_eq!(
            app.post_form(&lock, Some(&janitor), &[], "tags=1")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let page = app
            .get(&format!("/posts/{id}"), Some(&moderator))
            .await
            .body;
        assert!(page.contains(&lock), "{page}");
        let locked = app
            .post_form(
                &lock,
                Some(&moderator),
                &[],
                "rating=1&tags=1&notes=1&status=1",
            )
            .await;
        assert_eq!(locked.status, StatusCode::SEE_OTHER, "{}", locked.body);

        let edit = format!("/posts/{id}/edit");
        let refused = app
            .post_form(
                &edit,
                Some(&alice),
                &[],
                &form(&[("tags", "cat dog"), ("old_tags", "cat"), ("rating", "s")]),
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("tags are locked"), "{}", refused.body);
        let refused = app
            .post_form(
                &edit,
                Some(&alice),
                &[],
                &form(&[("tags", "cat"), ("old_tags", "cat"), ("rating", "e")]),
            )
            .await;
        assert!(
            refused.body.contains("rating is locked"),
            "{}",
            refused.body
        );
        // Other fields still change.
        let fine = app
            .post_form(
                &edit,
                Some(&alice),
                &[],
                &form(&[
                    ("tags", "cat"),
                    ("old_tags", "cat"),
                    ("rating", "s"),
                    ("source", "x"),
                ]),
            )
            .await;
        assert_eq!(fine.status, StatusCode::SEE_OTHER, "{}", fine.body);
        // Staff who lock posts aren't held back.
        let staff = app
            .post_form(
                &edit,
                Some(&moderator),
                &[],
                &form(&[("tags", "cat dog"), ("old_tags", "cat"), ("rating", "s")]),
            )
            .await;
        assert_eq!(staff.status, StatusCode::SEE_OTHER, "{}", staff.body);
        assert_eq!(tag_names(&pool, id).await, ["cat", "dog"]);

        let flag = app
            .post_form(&format!("/posts/{id}/flag"), Some(&alice), &[], "reason=x")
            .await;
        assert_eq!(flag.status, StatusCode::BAD_REQUEST);
        let delete = app
            .post_form(
                &format!("/posts/{id}/delete"),
                Some(&janitor),
                &[],
                "reason=x",
            )
            .await;
        assert_eq!(delete.status, StatusCode::BAD_REQUEST);
        assert!(delete.body.contains("status is locked"), "{}", delete.body);
        let state = test_state(&pool).await;
        let current = crate::test_support::current_user(&state, &alice).await;
        let note = crate::notes::create(
            &state,
            &current,
            id,
            moekura_core::notes::NoteBox {
                x: 1,
                y: 1,
                width: 5,
                height: 5,
            },
            "hi",
        )
        .await;
        assert!(
            matches!(&note, Err(crate::error::AppError::BadRequest(m)) if m.contains("notes are locked")),
            "{note:?}"
        );

        // The page and the history say so.
        let page = app.get(&format!("/posts/{id}"), Some(&alice)).await.body;
        assert!(
            page.contains("Locked: rating, tags, notes, status."),
            "{page}"
        );
        let history = app
            .get(&format!("/posts/{id}/history"), Some(&alice))
            .await
            .body;
        assert!(
            history.contains("locked: rating, tags, notes, status"),
            "{history}"
        );
        let logged: String =
            sqlx::query_scalar("SELECT action FROM mod_actions WHERE action = 'post.lock'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(logged, "post.lock");

        // Mass edits pass it by.
        let dog: i32 = sqlx::query_scalar("SELECT id FROM tags WHERE name = 'dog'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let changed = moekura_db::mass_updates::retag(&pool, &[id], &[], &[dog], None, None)
            .await
            .unwrap();
        assert_eq!(changed, 0);
        app.post_form(&lock, Some(&moderator), &[], "").await;
        let changed = moekura_db::mass_updates::retag(&pool, &[id], &[], &[dog], None, None)
            .await
            .unwrap();
        assert_eq!(changed, 1);
    }
}
