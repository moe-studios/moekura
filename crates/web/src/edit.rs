//! Editing posts: tags, rating, source, description and parent.

use axum::extract::{Path, Query};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use serde::Deserialize;
use uwuu_core::permissions::Permission;
use uwuu_core::posts::{DESCRIPTION_MAX_LEN, Rating, SOURCE_MAX_LEN};
use uwuu_core::tags::POST_MAX_TAGS;
use uwuu_db::posts::{self, PostEdit};
use uwuu_db::tags::{self, WantedTag};

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::{FailedEdit, render_post, visibility};
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
}

#[derive(Debug, Default, Deserialize)]
struct EditQuery {
    /// The search the post was opened from, kept across the edit.
    #[serde(default)]
    q: String,
}

/// Why an edit was refused, for the form.
enum Refused {
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
    match apply(&page, id, &form).await {
        Ok(()) => {
            let mut back = format!("/posts/{id}");
            if !query.q.is_empty() {
                let q = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("q", &query.q)
                    .finish();
                back = format!("{back}?{q}");
            }
            Ok((flash::set(jar, Flash::Saved), Redirect::to(&back)).into_response())
        }
        Err(Refused::Invalid(error)) => {
            render_post(
                &page,
                id,
                (!query.q.is_empty()).then_some(query.q.as_str()),
                true,
                Some(FailedEdit { form: &form, error }),
            )
            .await
        }
        Err(Refused::Error(error)) => Err(error),
    }
}

/// Validates `form` and saves it in one transaction with the post locked.
async fn apply(page: &Page, id: i64, form: &EditForm) -> Result<(), Refused> {
    let invalid = |message: &str| Err(Refused::Invalid(message.to_owned()));
    let db = page.state().db.primary();
    let Ok(rating) = form.rating.parse::<Rating>() else {
        return invalid("Choose a rating.");
    };
    let source = form.source.trim();
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
    let parent_id = match form.parent.trim().trim_start_matches('#') {
        "" => None,
        text => match text.parse::<i64>() {
            Ok(parent) if parent == id => return invalid("A post can't be its own parent."),
            Ok(parent) => Some(parent),
            Err(_) => return invalid("The parent must be a post number."),
        },
    };
    let changes = parse_edit(db, &form.old_tags, &form.tags).await?;

    let mut tx = db.begin().await?;
    let post = posts::lock(&mut *tx, id)
        .await?
        .ok_or(Refused::Error(AppError::NotFound))?;
    if !visibility(&page.current).allows(&post) {
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
    let current: Vec<String> = tags::by_ids(&mut *tx, &post.tag_ids)
        .await?
        .into_iter()
        .map(|t| t.name)
        .filter(|name| !changes.removed.contains(name))
        .collect();
    let mut wanted: Vec<WantedTag<'_>> = current
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
    let tag_ids: Vec<i32> =
        tags::for_post(&mut tx, &wanted, page.current.can(Permission::ManageTags))
            .await?
            .iter()
            .map(|t| t.id)
            .collect();

    uwuu_db::post_versions::attribute(&mut tx, page.current.user.as_ref().map(|u| u.id), None)
        .await?;
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
        user = page.current.user.as_ref().map(|u| u.name.as_str()),
        "post edited"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use sqlx::PgPool;
    use uwuu_core::permissions::SystemRole;

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

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
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
        let post = uwuu_db::posts::by_id(&pool, id).await.unwrap().unwrap();
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

    #[sqlx::test(migrator = "uwuu_db::MIGRATOR")]
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
            Some(format!("/posts/{child}?q=b").as_str())
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
            (parent, String::new(), "-bad", "may not start with `-`"),
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
}
