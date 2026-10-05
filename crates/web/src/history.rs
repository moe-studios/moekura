//! Post history and reverting to an earlier version.

use std::collections::HashMap;

use axum::Router;
use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::posts::{PostLock, Rating};
use moekura_db::post_versions::{self, Version};
use moekura_db::posts::{self, PostEdit};
use moekura_db::tags::{self, Tag, WantedTag};

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::visibility;
use crate::templates::search_url;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/posts/{id}/history", get(history))
        .route("/posts/{id}/revert/{version}", post(revert))
}

async fn history(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let post = posts::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if !visibility(&page.current).allows(&post) {
        return Err(AppError::NotFound);
    }
    let versions = post_versions::list(db, id).await?;

    let mut ids: Vec<i32> = versions
        .iter()
        .flat_map(|v| v.added_tag_ids.iter().chain(&v.removed_tag_ids).copied())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let categories = tags::categories(db).await?;
    let names: HashMap<i32, Tag> = tags::by_ids(db, &ids)
        .await?
        .into_iter()
        .map(|t| (t.id, t))
        .collect();
    let tag_list = |ids: &[i32]| -> Vec<Value> {
        let mut found: Vec<&Tag> = ids.iter().filter_map(|id| names.get(id)).collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
            .into_iter()
            .map(|t| {
                context! {
                    name => t.name,
                    category => categories.iter().find(|c| c.id == t.category_id).map(|c| c.name.clone()),
                    url => Value::from_safe_string(search_url(&t.name)),
                }
            })
            .collect()
    };

    let can_revert = page.current.can(Permission::EditPosts);
    // Newest first; each is compared with the one before it in time.
    let rows: Vec<Value> = versions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let previous = versions.get(i + 1);
            let changed = |f: fn(&Version) -> String| previous.is_none_or(|p| f(p) != f(v));
            context! {
                version => v.version,
                date => crate::dates::day(v.created_at),
                updater => v.updater_name,
                relation => v.relation_kind.as_ref().map(|kind| context! {
                    kind => kind,
                    antecedent => v.relation_antecedent,
                    consequent => v.relation_consequent,
                }),
                added => tag_list(&v.added_tag_ids),
                removed => tag_list(&v.removed_tag_ids),
                rating => changed(|v| v.rating.clone()).then(|| {
                    v.rating.parse::<Rating>().map(Rating::label).unwrap_or_default()
                }),
                // The first version only lists what was set.
                source => (changed(|v| v.source.clone()) && (previous.is_some() || !v.source.is_empty()))
                    .then(|| v.source.clone()),
                parent => (changed(|v| format!("{:?}", v.parent_id))
                    && (previous.is_some() || v.parent_id.is_some()))
                .then(|| v.parent_id.map_or_else(|| "none".to_owned(), |p| format!("#{p}"))),
                description_changed => previous.is_some() && changed(|v| v.description.clone()),
                locks => (changed(|v| v.locks.join(",")) && (previous.is_some() || !v.locks.is_empty()))
                    .then(|| if v.locks.is_empty() { "none".to_owned() } else { v.locks.join(", ") }),
                current => i == 0,
                can_revert => can_revert && i > 0,
            }
        })
        .collect();
    Ok(page.render("history.html", context! { post_id => id, versions => rows }))
}

/// Restores an earlier version's tags, rating, source, description and
/// parent. The restored state is itself a new version.
async fn revert(
    page: Page,
    jar: CookieJar,
    Path((id, version)): Path<(i64, i32)>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditPosts)?;
    let db = page.state().db.primary();
    let old = post_versions::get(db, id, version)
        .await?
        .ok_or(AppError::NotFound)?;
    let mut tx = db.begin().await?;
    let post = posts::lock(&mut *tx, id).await?.ok_or(AppError::NotFound)?;
    if !visibility(&page.current).allows(&post) {
        return Err(AppError::NotFound);
    }
    let old_tags = tags::by_ids(&mut *tx, &old.tag_ids).await?;
    // As the edit form refuses them: deprecated tags the post doesn't
    // have now can't come back.
    let mut deprecated: Vec<String> = old_tags
        .iter()
        .filter(|t| t.is_deprecated && !post.tag_ids.contains(&t.id))
        .map(|t| format!("`{}`", t.name))
        .collect();
    if !deprecated.is_empty() {
        deprecated.sort();
        return Err(AppError::Unprocessable(format!(
            "That version has tags that are deprecated now and can't be added back: {}.",
            deprecated.join(", ")
        )));
    }
    // Through aliases and implications, as if tagged again today.
    let names: Vec<String> = old_tags.into_iter().map(|t| t.name).collect();
    let wanted: Vec<WantedTag<'_>> = names
        .iter()
        .map(|name| WantedTag {
            name,
            category_id: None,
        })
        .collect();
    post_versions::attribute(&mut tx, page.current.user.as_ref().map(|u| u.id), None).await?;
    let tag_ids: Vec<i32> = tags::for_post(&mut tx, &wanted, false)
        .await?
        .iter()
        .map(|t| t.id)
        .collect();
    crate::artists::refuse_banned(
        page.state(),
        &mut *tx,
        &page.current,
        &tag_ids,
        &post.tag_ids,
    )
    .await
    .map_err(AppError::Unprocessable)?;
    // A parent deleted since then is dropped rather than failing.
    let parent_id = match old.parent_id {
        Some(parent) if posts::by_id(&mut *tx, parent).await?.is_some() => Some(parent),
        _ => None,
    };
    let rating = old
        .rating
        .parse::<Rating>()
        .map_err(|()| AppError::Internal(format!("stored rating `{}`", old.rating)))?;
    if tag_ids != post.tag_ids {
        crate::posts::check_lock(&page.current, &post, PostLock::Tags)?;
    }
    if rating != post.rating {
        crate::posts::check_lock(&page.current, &post, PostLock::Rating)?;
    }
    posts::update(
        &mut *tx,
        id,
        PostEdit {
            rating,
            source: &old.source,
            description: &old.description,
            parent_id,
            tag_ids: &tag_ids,
        },
    )
    .await?;
    tx.commit().await?;
    tracing::info!(post_id = id, version, "post reverted");
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/posts/{id}/history")),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn history_shows_changes_and_reverts(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            super::routes()
                .merge(crate::edit::routes())
                .merge(crate::posts::routes())
                .merge(crate::upload::routes(max)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let fields = vec![("rating", "s".to_owned()), ("tags", "cat cute".to_owned())];
        let response = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &fields,
                Some(("a.png", &fixture::png(20, 20))),
            )
            .await;
        let id: i64 = response.location.unwrap()["/posts/".len()..]
            .split('?')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let edit = "old_tags=cat+cute&tags=cat+dog&rating=e&source=https%3A%2F%2Fexample.com";
        app.post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], edit)
            .await;

        let page = app.get(&format!("/posts/{id}/history"), None).await;
        assert_eq!(page.status, StatusCode::OK);
        let body = &page.body;
        assert!(body.contains("Version 2"), "{body}");
        assert!(body.contains("class=\"added\""));
        assert!(body.contains(">dog<") && body.contains(">cute<"));
        assert!(body.contains("Explicit"));
        assert!(!body.contains("/revert/"), "visitors can't revert");

        let response = app
            .post(&format!("/posts/{id}/revert/1"), Some(&alice), &[])
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        let post = moekura_db::posts::by_id(&pool, id).await.unwrap().unwrap();
        assert_eq!((post.rating.code(), post.source.as_str()), ("s", ""));
        let versions = moekura_db::post_versions::list(&pool, id).await.unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].updater_name.as_deref(), Some("alice"));
        assert_eq!(
            app.post(&format!("/posts/{id}/revert/9"), Some(&alice), &[])
                .await
                .status,
            StatusCode::NOT_FOUND
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn reverting_brings_back_no_banned_or_deprecated_tags(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(
            state,
            super::routes()
                .merge(crate::edit::routes())
                .merge(crate::upload::routes(max)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        // Each uploaded, then edited to the second set of tags.
        let posts = [
            ("cat someone", "cat"),
            ("dog old", "dog"),
            ("bird old cute", "bird old"),
        ];
        let mut ids = Vec::new();
        for (i, (tags, edited)) in posts.into_iter().enumerate() {
            let fields = vec![("rating", "s".to_owned()), ("tags", tags.to_owned())];
            let png = fixture::png(20 + 4 * i as u32, 20);
            let response = app
                .post_multipart("/upload", Some(&alice), &fields, Some(("a.png", &png)))
                .await;
            let id: i64 = response.location.unwrap()["/posts/".len()..]
                .split('?')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            let form = format!("old_tags={tags}&tags={edited}&rating=s");
            let edit = app
                .post_form(&format!("/posts/{id}/edit"), Some(&alice), &[], &form)
                .await;
            assert_eq!(edit.status, StatusCode::SEE_OTHER, "{}", edit.body);
            ids.push(id);
        }
        sqlx::query("INSERT INTO artists (name, is_banned) VALUES ('someone', true)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE tags SET is_deprecated = true WHERE name = 'old'")
            .execute(&pool)
            .await
            .unwrap();
        let tags = async |id: i64| -> Vec<String> {
            sqlx::query_scalar(
                "SELECT t.name::text FROM posts p JOIN tags t ON t.id = ANY(p.tag_ids)
                 WHERE p.id = $1 ORDER BY t.name",
            )
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap()
        };
        let revert = async |id: i64| {
            app.post(&format!("/posts/{id}/revert/1"), Some(&alice), &[])
                .await
        };

        let banned = revert(ids[0]).await;
        assert_eq!(banned.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            banned.body.contains("the artist is banned"),
            "{}",
            banned.body
        );
        assert_eq!(tags(ids[0]).await, ["cat"]);
        let deprecated = revert(ids[1]).await;
        assert_eq!(deprecated.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            deprecated.body.contains("deprecated now"),
            "{}",
            deprecated.body
        );
        assert_eq!(tags(ids[1]).await, ["dog"]);
        // A deprecated tag the post still has doesn't stand in the way.
        let kept = revert(ids[2]).await;
        assert_eq!(kept.status, StatusCode::SEE_OTHER, "{}", kept.body);
        assert_eq!(tags(ids[2]).await, ["bird", "cute", "old"]);
    }
}
