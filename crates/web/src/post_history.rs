//! The sitewide post history (`/post_versions`), filtered by user, post,
//! tag and date, and undoing a user's edits for vandalism.

use std::collections::HashMap;

use axum::extract::Query;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::jobs::UndoUserEdits;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_core::posts::Rating;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::post_versions::{self, Change, Filter};
use moekura_db::tags::{self, Tag};
use moekura_db::{jobs, users};
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::moderation::{day_span, parse_day};
use crate::pages::Page;
use crate::posts::visibility;
use crate::templates::{search_url, url_value};

/// Versions per page.
const PAGE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/post_versions", get(index))
        .route("/post_versions/undo", post(undo))
}

#[derive(Debug, Default, Deserialize)]
struct VersionQuery {
    /// Who made the change.
    #[serde(default)]
    user: String,
    #[serde(default)]
    post: String,
    /// A tag the change added.
    #[serde(default)]
    added: String,
    /// A tag the change removed.
    #[serde(default)]
    removed: String,
    /// `YYYY-MM-DD`, UTC, inclusive.
    #[serde(default)]
    since: String,
    /// `YYYY-MM-DD`, UTC, inclusive.
    #[serde(default)]
    until: String,
    before: Option<i64>,
}

impl VersionQuery {
    fn url(&self, before: Option<i64>) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in [
            ("user", &self.user),
            ("post", &self.post),
            ("added", &self.added),
            ("removed", &self.removed),
            ("since", &self.since),
            ("until", &self.until),
        ] {
            if !value.trim().is_empty() {
                query.append_pair(key, value.trim());
            }
        }
        if let Some(before) = before {
            query.append_pair("before", &before.to_string());
        }
        format!("/post_versions?{}", query.finish())
    }
}

/// The id of the user or tag a filter names; -1 (matching nothing) for
/// a name nobody has.
async fn user_id(db: &sqlx::PgPool, name: &str) -> Result<Option<i64>, AppError> {
    match name.trim() {
        "" => Ok(None),
        name => Ok(Some(users::by_name(db, name).await?.map_or(-1, |u| u.id))),
    }
}

async fn tag_id(db: &sqlx::PgPool, name: &str) -> Result<Option<i32>, AppError> {
    match name.trim() {
        "" => Ok(None),
        name => Ok(Some(
            tags::by_name(db, &name.to_lowercase())
                .await?
                .map_or(-1, |t| t.id),
        )),
    }
}

async fn index(page: Page, Query(query): Query<VersionQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let (since, until) = day_span(parse_day(&query.since)?, parse_day(&query.until)?);
    let post_id = match query.post.trim().trim_start_matches('#') {
        "" => None,
        text => Some(
            text.parse()
                .map_err(|_| AppError::BadRequest("The post must be a number".into()))?,
        ),
    };
    let filter = Filter {
        updater_id: user_id(db, &query.user).await?,
        post_id,
        added_tag: tag_id(db, &query.added).await?,
        removed_tag: tag_id(db, &query.removed).await?,
        since,
        until,
        before: query.before,
    };
    let changes = post_versions::search(db, &filter, &visibility(&page.current), PAGE).await?;

    let mut ids: Vec<i32> = changes
        .iter()
        .flat_map(|c| {
            c.version
                .added_tag_ids
                .iter()
                .chain(&c.version.removed_tag_ids)
                .copied()
        })
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
    let rows: Vec<Value> = changes.iter().map(|c| row(c, &tag_list)).collect();
    let older = (changes.len() == PAGE as usize)
        .then(|| changes.last())
        .flatten()
        .map(|c| url_value(&query.url(Some(c.version.id))));
    let undo =
        (!query.user.trim().is_empty() && page.current.can(Permission::UndoEdits)).then(|| {
            context! {
                user => query.user.trim(),
                since => query.since.trim(),
                until => query.until.trim(),
            }
        });
    Ok(page.render(
        "post_versions.html",
        context! {
            versions => rows,
            older_url => older,
            undo => undo,
            query => context! {
                user => query.user,
                post => query.post,
                added => query.added,
                removed => query.removed,
                since => query.since,
                until => query.until,
            },
        },
    ))
}

/// What a version changed, for the page.
fn row(change: &Change, tag_list: &dyn Fn(&[i32]) -> Vec<Value>) -> Value {
    let v = &change.version;
    let upload = !change.has_previous;
    let changed = |previous: Option<&String>, now: &String| upload || previous != Some(now);
    context! {
        post_id => change.post_id,
        version => v.version,
        date => crate::dates::day(v.created_at),
        time => crate::dates::clock(v.created_at),
        updater => v.updater_name,
        relation => v.relation_kind.as_ref().map(|kind| context! {
            kind => kind,
            antecedent => v.relation_antecedent,
            consequent => v.relation_consequent,
        }),
        upload => upload,
        added => tag_list(&v.added_tag_ids),
        removed => tag_list(&v.removed_tag_ids),
        rating => changed(change.previous_rating.as_ref(), &v.rating)
            .then(|| v.rating.parse::<Rating>().map(Rating::label).unwrap_or_default()),
        source => (changed(change.previous_source.as_ref(), &v.source) && !(upload && v.source.is_empty()))
            .then(|| v.source.clone()),
        parent => ((upload && v.parent_id.is_some())
            || (!upload && change.previous_parent_id != v.parent_id))
            .then(|| v.parent_id.map_or_else(|| "none".to_owned(), |p| format!("#{p}"))),
        description_changed => !upload && change.previous_description.as_ref() != Some(&v.description),
        locks => (!upload && change.previous_locks.as_ref() != Some(&v.locks))
            .then(|| if v.locks.is_empty() { "none".to_owned() } else { v.locks.join(", ") }),
    }
}

#[derive(Debug, Deserialize)]
struct UndoForm {
    #[serde(default)]
    user: String,
    #[serde(default)]
    since: String,
    #[serde(default)]
    until: String,
}

/// Queues undoing every edit the user made in the range, and logs it.
async fn undo(
    page: Page,
    jar: CookieJar,
    Form(form): Form<UndoForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::UndoEdits)?;
    let db = page.state().db.primary();
    let user = users::by_name(db, form.user.trim())
        .await?
        .ok_or(AppError::NotFound)?;
    // Not someone of one's own rank or higher.
    let outranks = page
        .state()
        .site
        .get()
        .role(user.role_id)
        .is_none_or(|r| page.current.role.outranks(r));
    if !outranks {
        return Err(AppError::Forbidden);
    }
    let (since, until) = day_span(parse_day(&form.since)?, parse_day(&form.until)?);
    let actor = page.current.user.as_ref().map(|u| u.id);
    let mut tx = db.begin().await?;
    jobs::enqueue(
        &mut tx,
        &UndoUserEdits {
            user_id: user.id,
            since: since.map(|t| t.unix_timestamp()),
            until: until.map(|t| t.unix_timestamp()),
            actor_id: actor,
        },
    )
    .await?;
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::UndoEdits)
            .user(user.id)
            .details(serde_json::json!({
                "since": form.since.trim(),
                "until": form.until.trim(),
            })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(user = user.name, "undoing a user's post edits");
    let back = VersionQuery {
        user: user.name.clone(),
        since: form.since,
        until: form.until,
        ..VersionQuery::default()
    }
    .url(None);
    Ok((flash::set(jar, Flash::Queued), Redirect::to(&back)).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn the_sitewide_history_and_undoing_a_user(pool: PgPool) {
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
        let vandal = session_for(&pool, "vandal", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let response = app
            .post_multipart(
                "/upload",
                Some(&alice),
                &[("rating", "s".to_owned()), ("tags", "cat".to_owned())],
                Some(("a.png", &fixture::png(20, 20))),
            )
            .await;
        let id: i64 = response.location.unwrap()["/posts/".len()..]
            .parse()
            .unwrap();
        let edit = app
            .post_form(
                &format!("/posts/{id}/edit"),
                Some(&vandal),
                &[],
                "tags=junk&old_tags=cat&rating=e",
            )
            .await;
        assert_eq!(edit.status, StatusCode::SEE_OTHER, "{}", edit.body);

        let all = app.get("/post_versions", Some(&alice)).await;
        assert_eq!(all.status, StatusCode::OK);
        assert!(
            all.body.contains(">junk</a>") && all.body.contains(">cat</a>"),
            "{}",
            all.body
        );
        let theirs = app
            .get("/post_versions?user=vandal", Some(&alice))
            .await
            .body;
        assert!(theirs.contains("rating: Explicit"), "{theirs}");
        assert!(
            !theirs.contains("/post_versions/undo"),
            "members can't undo"
        );
        let added = app.get("/post_versions?added=cat", Some(&alice)).await.body;
        assert!(
            added.contains(">alice</a>") && !added.contains(">vandal</a>"),
            "{added}"
        );
        let nobody = app
            .get("/post_versions?user=nobody", Some(&alice))
            .await
            .body;
        assert!(nobody.contains("No changes"), "{nobody}");

        let page = app
            .get("/post_versions?user=vandal", Some(&moderator))
            .await
            .body;
        assert!(page.contains("/post_versions/undo"), "{page}");
        assert_eq!(
            app.post_form("/post_versions/undo", Some(&alice), &[], "user=vandal")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let queued = app
            .post_form("/post_versions/undo", Some(&moderator), &[], "user=vandal")
            .await;
        assert_eq!(queued.status, StatusCode::SEE_OTHER, "{}", queued.body);
        let job: serde_json::Value =
            sqlx::query_scalar("SELECT payload FROM jobs WHERE kind = 'post_versions.undo_user'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(job["since"], serde_json::Value::Null);
        let logged: String = sqlx::query_scalar(
            "SELECT action FROM mod_actions WHERE action = 'post_versions.undo'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(logged, "post_versions.undo");
        // Not staff of one's own rank.
        session_for(&pool, "mod2", SystemRole::Moderator).await;
        assert_eq!(
            app.post_form("/post_versions/undo", Some(&moderator), &[], "user=mod2")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn undoing_takes_its_own_permission(pool: PgPool) {
        // Moderators who may mass edit tags but not undo edits.
        sqlx::query(
            "UPDATE roles SET permissions = permissions & ~(1::bigint << 23)
             WHERE system_key = 'moderator'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let app = TestApp::new(test_state(&pool).await, super::routes());
        session_for(&pool, "vandal", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let page = app
            .get("/post_versions?user=vandal", Some(&moderator))
            .await
            .body;
        assert!(!page.contains("/post_versions/undo"), "{page}");
        assert_eq!(
            app.post_form("/post_versions/undo", Some(&moderator), &[], "user=vandal")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
    }
}
