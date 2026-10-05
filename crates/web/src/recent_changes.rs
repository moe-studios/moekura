//! Sitewide recent changes to wiki pages (`/wiki_page_versions`), pools
//! (`/pool_versions`) and notes (`/note_versions`), each filterable by
//! user. Items have their own history pages; these list every item's.

use axum::Router;
use axum::extract::Query;
use axum::response::Response;
use axum::routing::get;
use minijinja::{Value, context};
use moekura_core::permissions::Permission;
use moekura_core::pools::{PoolName, post_changes};
use moekura_db::{notes, pools, users, wiki};
use serde::Deserialize;
use time::OffsetDateTime;

use crate::AppState;
use crate::error::AppError;
use crate::pages::Page;
use crate::templates::url_value;

/// Versions per page.
const PAGE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/wiki_page_versions", get(wiki_changes))
        .route("/pool_versions", get(pool_changes))
        .route("/note_versions", get(note_changes))
}

#[derive(Debug, Default, Deserialize)]
struct ChangeQuery {
    /// Who made the change.
    #[serde(default)]
    user: String,
    before: Option<i64>,
}

impl ChangeQuery {
    /// The user filter's id: -1 (matching nothing) for a name nobody has.
    async fn updater_id(&self, db: &sqlx::PgPool) -> Result<Option<i64>, AppError> {
        match self.user.trim() {
            "" => Ok(None),
            name => Ok(Some(users::by_name(db, name).await?.map_or(-1, |u| u.id))),
        }
    }

    /// The next page's link, when this one is full.
    fn older_url(&self, path: &str, ids: &[i64]) -> Option<Value> {
        if ids.len() < PAGE as usize {
            return None;
        }
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        if !self.user.trim().is_empty() {
            query.append_pair("user", self.user.trim());
        }
        query.append_pair("before", &ids.last()?.to_string());
        Some(url_value(&format!("{path}?{}", query.finish())))
    }
}

/// Which list a page shows, for the shared template.
struct Kind {
    path: &'static str,
}

const WIKI: Kind = Kind {
    path: "/wiki_page_versions",
};
const POOLS: Kind = Kind {
    path: "/pool_versions",
};
const NOTES: Kind = Kind {
    path: "/note_versions",
};

fn when(at: OffsetDateTime) -> (String, String) {
    (
        at.date().to_string(),
        format!("{:02}:{:02}", at.hour(), at.minute()),
    )
}

fn render(
    page: &Page,
    kind: &Kind,
    query: &ChangeQuery,
    rows: Vec<Value>,
    ids: &[i64],
) -> Response {
    page.render(
        "recent_changes.html",
        context! {
            kind => kind.path.trim_start_matches('/'),
            path => kind.path,
            versions => rows,
            older_url => query.older_url(kind.path, ids),
            query => context! { user => query.user },
        },
    )
}

async fn wiki_changes(page: Page, Query(query): Query<ChangeQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let updater_id = query.updater_id(db).await?;
    let changes = wiki::recent_versions(db, updater_id, query.before, PAGE).await?;
    let rows = changes
        .iter()
        .map(|c| {
            let (date, time) = when(c.created_at);
            let url = moekura_core::markup::wiki_url(&c.title);
            context! {
                title => c.title,
                url => Value::from_safe_string(url.clone()),
                version => c.version,
                version_url => Value::from_safe_string(format!("{url}?version={}", c.version)),
                date => date,
                time => time,
                updater => c.updater_name,
                created => c.previous_length.is_none(),
                change => c.previous_length.map(|before| c.length - before),
            }
        })
        .collect();
    let ids: Vec<i64> = changes.iter().map(|c| c.id).collect();
    Ok(render(&page, &WIKI, &query, rows, &ids))
}

async fn pool_changes(page: Page, Query(query): Query<ChangeQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let updater_id = query.updater_id(db).await?;
    let deleted = crate::pools::sees_deleted(&page.current);
    let changes = pools::recent_versions(db, updater_id, deleted, query.before, PAGE).await?;
    let rows = changes
        .iter()
        .map(|c| {
            let (v, p) = (&c.version, &c.previous);
            let (date, time) = when(v.created_at);
            let created = p.previous_post_ids.is_none();
            let before = p.previous_post_ids.as_deref().unwrap_or_default();
            let (added, removed) = post_changes(before, &v.post_ids);
            let reordered =
                !created && added.is_empty() && removed.is_empty() && before != v.post_ids;
            context! {
                pool_id => c.pool_id,
                name => PoolName::display(&v.name),
                version => v.version,
                date => date,
                time => time,
                updater => v.updater_name,
                created => created,
                renamed_from => p.previous_name.as_ref().filter(|&n| *n != v.name).map(|n| PoolName::display(n)),
                category => p.previous_category.as_ref().is_some_and(|c| *c != v.category).then_some(&v.category),
                description_changed => p.previous_description.as_ref().is_some_and(|d| *d != v.description),
                deleted => p.previous_is_deleted.filter(|&d| d != v.is_deleted).map(|_| v.is_deleted),
                added => added,
                removed => removed,
                reordered => reordered,
            }
        })
        .collect();
    let ids: Vec<i64> = changes.iter().map(|c| c.id).collect();
    Ok(render(&page, &POOLS, &query, rows, &ids))
}

async fn note_changes(page: Page, Query(query): Query<ChangeQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let updater_id = query.updater_id(db).await?;
    let visibility = crate::posts::visibility(&page.current);
    let changes = notes::recent_versions(db, updater_id, &visibility, query.before, PAGE).await?;
    let rows = changes
        .iter()
        .map(|c| {
            let v = &c.version;
            let (date, time) = when(v.created_at);
            let created = c.previous_body.is_none();
            context! {
                note_id => v.note_id,
                post_id => v.post_id,
                version => v.version,
                date => date,
                time => time,
                updater => v.updater_name,
                created => created,
                body => (created || c.previous_body.as_ref() != Some(&v.body)).then_some(&v.body),
                moved => c.moved,
                deleted => c.previous_is_active.filter(|&a| a != v.is_active).map(|_| !v.is_active),
            }
        })
        .collect();
    let ids: Vec<i64> = changes.iter().map(|c| c.id).collect();
    Ok(render(&page, &NOTES, &query, rows, &ids))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::notes::NoteBox;
    use moekura_core::permissions::SystemRole;
    use moekura_db::notes::Changes;
    use moekura_db::pools::Contents;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    async fn user_id(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn wiki_changes(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        session_for(&pool, "alice", SystemRole::Member).await;
        session_for(&pool, "bob", SystemRole::Member).await;
        let (alice, bob) = (user_id(&pool, "alice").await, user_id(&pool, "bob").await);
        moekura_db::wiki::save(&pool, "cat", "A cat.", Some(alice), Some(0))
            .await
            .unwrap();
        moekura_db::wiki::save(&pool, "cat", "A small cat.", Some(bob), Some(1))
            .await
            .unwrap();
        moekura_db::wiki::save(&pool, "dog", "A dog.", Some(alice), Some(0))
            .await
            .unwrap();

        let all = app.get("/wiki_page_versions", None).await;
        assert_eq!(all.status, StatusCode::OK);
        assert!(
            all.body.contains(">dog</a>") && all.body.contains("+6 characters"),
            "{}",
            all.body
        );
        let theirs = app.get("/wiki_page_versions?user=bob", None).await.body;
        assert!(
            theirs.contains(">cat</a>") && !theirs.contains(">dog</a>"),
            "{theirs}"
        );
        let nobody = app.get("/wiki_page_versions?user=carol", None).await.body;
        assert!(nobody.contains("No changes"), "{nobody}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn pool_changes_hide_deleted_pools(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let post: i64 = sqlx::query_scalar("INSERT INTO posts (rating) VALUES ('g') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut contents = Contents {
            name: "My_Comic".into(),
            description: String::new(),
            category: "series".into(),
            is_deleted: false,
            post_ids: Vec::new(),
        };
        let comic = moekura_db::pools::create(&pool, &contents, None)
            .await
            .unwrap();
        contents.post_ids = vec![post];
        moekura_db::pools::save(&pool, comic, &contents, None, None)
            .await
            .unwrap();
        contents.name = "Gone".into();
        let gone = moekura_db::pools::create(&pool, &contents, None)
            .await
            .unwrap();
        contents.is_deleted = true;
        moekura_db::pools::save(&pool, gone, &contents, None, None)
            .await
            .unwrap();

        let all = app.get("/pool_versions", None).await.body;
        assert!(
            all.contains("My Comic") && all.contains(&format!("+<a href=\"/posts/{post}\">")),
            "{all}"
        );
        assert!(!all.contains("Gone"), "{all}");
        let staff = app.get("/pool_versions", Some(&moderator)).await.body;
        assert!(
            staff.contains("Gone") && staff.contains("deleted"),
            "{staff}"
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn note_changes_follow_post_visibility(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let alice_id = user_id(&pool, "alice").await;
        let post = |status: &'static str| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i64>(
                    "INSERT INTO posts (rating, status) VALUES ('g', $1) RETURNING id",
                )
                .bind(status)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let (shown, hidden) = (post("active").await, post("deleted").await);
        let note_box = NoteBox {
            x: 1,
            y: 1,
            width: 10,
            height: 10,
        };
        let id = moekura_db::notes::create(&pool, shown, note_box, "Hello", Some(alice_id))
            .await
            .unwrap();
        let moved = Changes {
            note_box: Some(NoteBox { x: 5, ..note_box }),
            ..Changes::default()
        };
        moekura_db::notes::update(&pool, id, &moved, None, None)
            .await
            .unwrap();
        moekura_db::notes::create(&pool, hidden, note_box, "Secret", Some(alice_id))
            .await
            .unwrap();

        let all = app.get("/note_versions", Some(&alice)).await.body;
        assert!(
            all.contains("Hello") && all.contains("moved") && !all.contains("Secret"),
            "{all}"
        );
        let theirs = app.get("/note_versions?user=alice", None).await.body;
        assert!(
            theirs.contains("Hello") && !theirs.contains("moved"),
            "{theirs}"
        );
    }
}
