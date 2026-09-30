//! The related tags panel of the upload and edit forms (`/tags/related`):
//! tags often used with the box's tags or a chosen one, the user's recent
//! and frequent tags, tags whose wiki pages list a word in the box as an
//! other name, and the chosen tag's wiki links. JSON for the script,
//! which updates the panel as the tags change; a page without scripts.

use std::collections::HashMap;

use axum::Json;
use axum::Router;
use axum::extract::Query;
use axum::http::HeaderMap;
use axum::http::header::{ACCEPT, CACHE_CONTROL};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use minijinja::context;
use moekura_core::permissions::Permission;
use moekura_core::search::{Query as SearchQuery, TagTerm};
use moekura_db::search::{PageRef, Plan};
use moekura_db::tags::{self, Category, Tag};
use moekura_db::{post_versions, wiki};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::pages::Page;
use crate::posts::visibility;

/// Posts looked at for tags used together: the newest that match.
pub(crate) const SAMPLE: u32 = 200;
/// Tags in each group.
const GROUP_SIZE: usize = 25;
/// Box tags searched together for the related group: the least used.
const SEARCHED_TAGS: usize = 4;

pub fn routes() -> Router<AppState> {
    Router::new().route("/tags/related", get(related))
}

#[derive(Debug, Default, Deserialize)]
struct RelatedQuery {
    /// The tag box, as typed.
    #[serde(default)]
    tags: String,
    /// A tag to relate to instead of the whole box, and whose wiki links
    /// to list.
    #[serde(default)]
    tag: String,
}

#[derive(Debug, Serialize)]
struct RelatedTag {
    name: String,
    category: String,
    post_count: i32,
    /// Already in the box.
    selected: bool,
    /// For translations: the word in the box it translates.
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
}

#[derive(Debug, Serialize)]
struct Group {
    /// `related`, `translated`, `recent`, `frequent` or `wiki`.
    kind: &'static str,
    title: String,
    tags: Vec<RelatedTag>,
}

#[derive(Debug, Serialize)]
struct Panel {
    groups: Vec<Group>,
}

/// How many of the newest (at most [`SAMPLE`]) posts matching `query`
/// carry each tag, most first: the sample's size and the counts.
pub(crate) async fn co_occurring(
    state: &AppState,
    current: &CurrentUser,
    query: &str,
) -> Result<(usize, Vec<(Tag, i64)>), AppError> {
    let db = state.reader(current);
    let mut query =
        SearchQuery::parse(query.trim()).map_err(|e| AppError::Unprocessable(e.to_string()))?;
    query.limit = Some(SAMPLE.min(state.config.search.max_per_page));
    let search_error = |e| match e {
        moekura_db::search::SearchError::Invalid(message) => AppError::Unprocessable(message),
        moekura_db::search::SearchError::Db(e) => e.into(),
    };
    let plan = Plan::resolve(db, &query, &visibility(current), &state.config.search)
        .await
        .map_err(search_error)?;
    let ids = plan
        .ids(db, PageRef::default())
        .await
        .map_err(search_error)?;
    let counts = tags::counts_among(db, &ids, 500).await?;
    let mut found: HashMap<i32, Tag> =
        tags::by_ids(db, &counts.iter().map(|(id, _)| *id).collect::<Vec<_>>())
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect();
    let counted = counts
        .into_iter()
        .filter_map(|(id, n)| found.remove(&id).map(|t| (t, n)))
        .collect();
    Ok((ids.len(), counted))
}

/// The tag names typed in `input` (metatags and `-tag` left out).
fn box_tags(input: &str, categories: &[Category]) -> Vec<String> {
    let names: Vec<&str> = categories.iter().map(|c| c.name.as_str()).collect();
    moekura_core::post_edit::parse(input, &names, &[])
        .tags
        .into_iter()
        .map(|t| t.name.into_string())
        .collect()
}

async fn related(
    page: Page,
    headers: HeaderMap,
    Query(query): Query<RelatedQuery>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let current = &page.current;
    let db = state.reader(current);
    let categories = tags::categories(db).await?;
    let in_box = box_tags(&query.tags, &categories);
    let chosen = moekura_core::tags::TagName::parse(&query.tag)
        .ok()
        .map(|t| t.into_string());
    let item = |tag: &Tag| RelatedTag {
        name: tag.name.clone(),
        category: crate::tags::category_name(&categories, tag.category_id),
        post_count: tag.post_count,
        selected: in_box.contains(&tag.name),
        from: None,
    };
    let mut groups = Vec::new();

    // Tags used with the chosen tag, or with the box's most telling tags.
    let known = tags::by_names(db, &in_box.iter().map(String::as_str).collect::<Vec<_>>()).await?;
    let (search, title) = match &chosen {
        Some(tag) => (tag.clone(), format!("Related to {tag}")),
        None => {
            let mut used: Vec<&Tag> = known.iter().filter(|t| t.post_count > 0).collect();
            used.sort_by_key(|t| t.post_count);
            let terms: Vec<String> = used
                .iter()
                .take(SEARCHED_TAGS)
                .map(|t| {
                    if used.len() > 1 {
                        format!("~{}", t.name)
                    } else {
                        t.name.clone()
                    }
                })
                .collect();
            (terms.join(" "), "Related".to_owned())
        }
    };
    if !search.is_empty() {
        let searched = SearchQuery::parse(&search)
            .map(|q| {
                q.all
                    .iter()
                    .chain(&q.any)
                    .filter_map(|t| match t {
                        TagTerm::Name(name) => Some(name.as_str().to_owned()),
                        TagTerm::Wildcard(_) => None,
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (_, counted) = co_occurring(state, current, &search).await?;
        let tags: Vec<RelatedTag> = counted
            .iter()
            .filter(|(t, _)| !searched.contains(&t.name))
            .take(GROUP_SIZE)
            .map(|(t, _)| item(t))
            .collect();
        if !tags.is_empty() {
            groups.push(Group {
                kind: "related",
                title,
                tags,
            });
        }
    }

    // Words that aren't tags but are some page's other name.
    let unknown: Vec<String> = query
        .tags
        .split_whitespace()
        .filter(|w| !w.starts_with('-') && !w.contains(':'))
        .map(moekura_core::wiki::normalize_other_name)
        .filter(|w| {
            let tag = moekura_core::tags::normalize(w);
            !known.iter().any(|t| t.name == tag)
        })
        .collect();
    if !unknown.is_empty() {
        let pages = wiki::titles_for_other_names(db, &unknown).await?;
        let titles: Vec<&str> = pages.iter().map(|(title, _)| title.as_str()).collect();
        let found = tags::by_names(db, &titles).await?;
        let tags: Vec<RelatedTag> = pages
            .iter()
            .filter_map(|(title, names)| {
                let tag = found.iter().find(|t| t.name == *title)?;
                let from = unknown.iter().find(|w| names.contains(w))?.clone();
                Some(RelatedTag {
                    from: Some(from),
                    ..item(tag)
                })
            })
            .take(GROUP_SIZE)
            .collect();
        if !tags.is_empty() {
            groups.push(Group {
                kind: "translated",
                title: "Translated".to_owned(),
                tags,
            });
        }
    }

    // The editor's own habits.
    if let Some(user) = &current.user {
        for (kind, title, ids) in [
            (
                "recent",
                "Your recent tags",
                post_versions::recent_tags(db, user.id, GROUP_SIZE as i64).await?,
            ),
            (
                "frequent",
                "Your frequent tags",
                post_versions::frequent_tags(db, user.id, GROUP_SIZE as i64).await?,
            ),
        ] {
            let mut found = tags::by_ids(db, &ids).await?;
            found.sort_by_key(|t| ids.iter().position(|id| *id == t.id));
            let tags: Vec<RelatedTag> = found.iter().map(item).collect();
            if !tags.is_empty() {
                groups.push(Group {
                    kind,
                    title: title.to_owned(),
                    tags,
                });
            }
        }
    }

    // The chosen tag's wiki links.
    if let Some(tag) = &chosen
        && let Some(wiki_page) = wiki::by_title(db, tag).await?
    {
        let links = moekura_core::markup::wiki_links(&wiki_page.body);
        let found =
            tags::by_names(db, &links.iter().map(String::as_str).collect::<Vec<_>>()).await?;
        let tags: Vec<RelatedTag> = links
            .iter()
            .filter_map(|name| found.iter().find(|t| t.name == *name))
            .take(GROUP_SIZE)
            .map(item)
            .collect();
        if !tags.is_empty() {
            groups.push(Group {
                kind: "wiki",
                title: format!("From the {tag} wiki page"),
                tags,
            });
        }
    }

    let panel = Panel { groups };
    let wants_json = headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"));
    if wants_json {
        // Private: what the viewer sees and their own tags.
        return Ok(([(CACHE_CONTROL, "private, max-age=60")], Json(panel)).into_response());
    }
    let groups: Vec<_> = panel
        .groups
        .iter()
        .map(|g| {
            context! {
                kind => g.kind,
                title => g.title,
                tags => g.tags.iter().map(|t| context! {
                    name => t.name,
                    category => t.category,
                    count => t.post_count,
                    selected => t.selected,
                    from => t.from,
                    url => minijinja::Value::from_safe_string(crate::templates::search_url(&t.name)),
                }).collect::<Vec<_>>(),
            }
        })
        .collect();
    Ok(page.render(
        "related_tags.html",
        context! {
            groups => groups,
            query => context! { tags => query.tags, tag => query.tag },
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn panel_groups(pool: PgPool) {
        let state = test_state(&pool).await;
        let max = state.config.media.max_upload_mb * 1024 * 1024;
        let app = TestApp::new(state, super::routes().merge(crate::upload::routes(max)));
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        for (width, tags) in [
            (20, "cat whiskers"),
            (24, "cat whiskers paws"),
            (28, "dog paws"),
        ] {
            let fields = vec![("rating", "s".to_owned()), ("tags", tags.to_owned())];
            let response = app
                .post_multipart(
                    "/upload",
                    Some(&alice),
                    &fields,
                    Some(("a.png", &fixture::png(width, 20))),
                )
                .await;
            assert_eq!(response.status, StatusCode::SEE_OTHER, "{}", response.body);
        }
        let names = vec!["猫".to_owned()];
        moekura_db::wiki::save(
            &pool,
            "cat",
            moekura_db::wiki::Text {
                body: "Felines. See [[whiskers]], [[paws]] and [[nothing_yet]].",
                other_names: Some(&names),
            },
            None,
            None,
        )
        .await
        .unwrap();

        let json = |path: &str, session: Option<&str>| {
            let path = path.to_owned();
            let session = session.map(str::to_owned);
            let app = &app;
            async move {
                let response = app.get_json(&path, session.as_deref()).await;
                assert_eq!(response.status, StatusCode::OK, "{}", response.body);
                serde_json::from_str::<Value>(&response.body).unwrap()
            }
        };
        let group = |panel: &Value, kind: &str| -> Vec<String> {
            panel["groups"]
                .as_array()
                .unwrap()
                .iter()
                .find(|g| g["kind"] == kind)
                .map(|g| {
                    g["tags"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|t| t["name"].as_str().unwrap().to_owned())
                        .collect()
                })
                .unwrap_or_default()
        };

        let panel = json("/tags/related?tags=cat+%E7%8C%AB&tag=cat", Some(&alice)).await;
        assert_eq!(group(&panel, "related")[0], "whiskers");
        assert_eq!(group(&panel, "translated"), ["cat"]);
        assert_eq!(group(&panel, "wiki"), ["whiskers", "paws"]);
        assert!(group(&panel, "recent").contains(&"dog".to_owned()));
        assert!(group(&panel, "frequent").contains(&"cat".to_owned()));
        let selected = &panel["groups"][1]["tags"][0];
        assert_eq!(
            (&selected["selected"], &selected["from"]),
            (&Value::Bool(true), &Value::from("猫"))
        );

        // The whole box, for visitors: no habits of their own.
        let panel = json("/tags/related?tags=paws", None).await;
        assert_eq!(group(&panel, "related").len(), 3);
        assert!(group(&panel, "recent").is_empty());

        // Without scripts, a page.
        let page = app.get("/tags/related?tags=cat&tag=cat", None).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains(">whiskers</a>"), "{}", page.body);
    }
}
