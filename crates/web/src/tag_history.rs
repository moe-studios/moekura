//! Tag history (`/tag_versions`): creation and every change to a tag's
//! name, category or deprecation, sitewide or for one tag, filtered by
//! user.

use axum::Router;
use axum::extract::Query;
use axum::response::Response;
use axum::routing::get;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_db::tag_versions::{self, Change, Filter};
use moekura_db::tags::{self, Category};
use moekura_db::users;
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;
use crate::pages::Page;
use crate::templates::{search_url, url_value};

/// Versions per page.
const PAGE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new().route("/tag_versions", get(index))
}

#[derive(Debug, Default, Deserialize)]
struct VersionQuery {
    #[serde(default)]
    tag: String,
    /// Who made the change.
    #[serde(default)]
    user: String,
    before: Option<i64>,
}

impl VersionQuery {
    fn url(&self, before: Option<i64>) -> String {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in [("tag", &self.tag), ("user", &self.user)] {
            if !value.trim().is_empty() {
                query.append_pair(key, value.trim());
            }
        }
        if let Some(before) = before {
            query.append_pair("before", &before.to_string());
        }
        format!("/tag_versions?{}", query.finish())
    }
}

async fn index(page: Page, Query(query): Query<VersionQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    // A name nobody has matches nothing.
    let tag_id = match moekura_core::tags::normalize(&query.tag).as_str() {
        "" => None,
        name => Some(tags::by_name(db, name).await?.map_or(-1, |t| t.id)),
    };
    let updater_id = match query.user.trim() {
        "" => None,
        name => Some(users::by_name(db, name).await?.map_or(-1, |u| u.id)),
    };
    let filter = Filter {
        tag_id,
        updater_id,
        before: query.before,
        offset: 0,
    };
    let changes = tag_versions::search(db, &filter, PAGE).await?;
    let categories = tags::categories(db).await?;
    let rows: Vec<Value> = changes.iter().map(|c| row(c, &categories)).collect();
    let older = (changes.len() == PAGE as usize)
        .then(|| changes.last())
        .flatten()
        .map(|c| url_value(&query.url(Some(c.id))));
    Ok(page.render(
        "tag_versions.html",
        context! {
            versions => rows,
            older_url => older,
            query => context! { tag => query.tag, user => query.user },
        },
    ))
}

/// What a version changed, for the page.
fn row(change: &Change, categories: &[Category]) -> Value {
    let created = change.previous_id.is_none();
    let label = |id: i16| {
        categories
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| id.to_string(), |c| c.label.clone())
    };
    let category = crate::tags::category_name(categories, change.category_id);
    context! {
        tag_id => change.tag_id,
        name => change.name,
        category => category,
        url => Value::from_safe_string(search_url(&change.name)),
        version => change.version,
        date => change.created_at.date().to_string(),
        time => format!("{:02}:{:02}", change.created_at.hour(), change.created_at.minute()),
        updater => change.updater_name,
        created => created,
        renamed_from => change.previous_name.as_ref().filter(|&n| *n != change.name),
        category_label => (created || change.previous_category_id != Some(change.category_id))
            .then(|| label(change.category_id)),
        previous_category_label => change
            .previous_category_id
            .filter(|&id| id != change.category_id)
            .map(label),
        deprecated => (created || change.previous_is_deprecated != Some(change.is_deprecated))
            .then_some(change.is_deprecated),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn lists_tag_changes(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::tags::routes()),
        );
        let id: i32 = sqlx::query_scalar("INSERT INTO tags (name) VALUES ('someone') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO tags (name) VALUES ('other')")
            .execute(&pool)
            .await
            .unwrap();
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let saved = app
            .post_form(
                &format!("/tags/{id}/edit"),
                Some(&admin),
                &[],
                "category=1&deprecated=on",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);

        let all = app.get("/tag_versions", None).await;
        assert_eq!(all.status, StatusCode::OK);
        assert!(all.body.contains(">other</a>"), "{}", all.body);
        let one = app.get("/tag_versions?tag=Someone", None).await.body;
        assert!(!one.contains(">other</a>"), "{one}");
        assert!(one.contains("category: General → Artist"), "{one}");
        assert!(one.contains("deprecated"), "{one}");
        assert!(one.contains(">root</a>"), "{one}");
        assert!(one.contains("created"), "{one}");
        let theirs = app.get("/tag_versions?user=root", None).await.body;
        assert!(
            theirs.contains("Artist") && !theirs.contains(">other</a>"),
            "{theirs}"
        );
        let nobody = app.get("/tag_versions?user=nobody", None).await.body;
        assert!(nobody.contains("No changes"), "{nobody}");
    }
}
