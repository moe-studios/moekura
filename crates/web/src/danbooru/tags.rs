//! `/tags.json`, `/autocomplete.json`, `/tag_aliases.json`,
//! `/tag_implications.json` and `/related_tag.json`.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_core::search::{Query as SearchQuery, TagTerm};
use moekura_core::tags::normalize;
use moekura_db::search::{PageRef, Plan};
use moekura_db::tag_relations::{self, Kind, Relation, Status};
use moekura_db::tags::{self, Category, ListOrder, Tag, TagFilter};
use serde::{Deserialize, Serialize};

use super::{ListParams, json};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::posts::visibility;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/tags", get(index))
        .route("/autocomplete", get(autocomplete))
        .route("/tag_aliases", get(aliases))
        .route("/tag_implications", get(implications))
        .route("/related_tag", get(related))
}

/// A tag as Danbooru describes it.
#[derive(Debug, Serialize)]
struct DanbooruTag {
    id: i32,
    name: String,
    post_count: i32,
    /// Moekura's category ids are Danbooru's.
    category: i16,
    created_at: String,
    updated_at: String,
    is_deprecated: bool,
    words: Vec<String>,
}

impl From<Tag> for DanbooruTag {
    fn from(tag: Tag) -> Self {
        let created = super::timestamp(tag.created_at);
        Self {
            words: tag
                .name
                .split('_')
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect(),
            id: tag.id,
            post_count: tag.post_count,
            category: tag.category_id,
            updated_at: created.clone(),
            created_at: created,
            is_deprecated: tag.is_deprecated,
            name: tag.name,
        }
    }
}

/// The page and limit of a list as an offset and count.
fn window(list: &ListParams, max: u32) -> Result<(i64, i64), AppError> {
    let limit = i64::from(list.limit(max));
    let page: i64 = match list.page.trim() {
        "" => 1,
        page => page
            .parse()
            .ok()
            .filter(|&n: &i64| (1..=1000).contains(&n))
            .ok_or_else(|| AppError::BadRequest("`page` must be a number from 1 to 1000".into()))?,
    };
    Ok(((page - 1) * limit, limit))
}

/// A category given by name or Danbooru id, or a comma list of them.
fn category_ids(value: &str, categories: &[Category]) -> Vec<i16> {
    value
        .split(',')
        .map(str::trim)
        .filter_map(|c| {
            categories
                .iter()
                .find(|cat| cat.name == c || c.parse() == Ok(cat.id))
                .map(|cat| cat.id)
        })
        .collect()
}

fn yes(value: &str) -> bool {
    matches!(value.trim(), "yes" | "true" | "1")
}

#[derive(Debug, Default, Deserialize)]
struct TagParams {
    #[serde(rename = "search[name_matches]", default)]
    name_matches: String,
    #[serde(rename = "search[name]", default)]
    name: String,
    #[serde(rename = "search[name_comma]", default)]
    name_comma: String,
    #[serde(rename = "search[category]", default)]
    category: String,
    #[serde(rename = "search[order]", default)]
    order: String,
    #[serde(rename = "search[hide_empty]", default)]
    hide_empty: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<TagParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let (offset, limit) = window(&params.list, 1000)?;
    let categories = tags::categories(db).await?;
    let names: Vec<String> = [params.name.as_str(), params.name_comma.as_str()]
        .iter()
        .flat_map(|list| list.split(','))
        .map(normalize)
        .filter(|n| !n.is_empty())
        .collect();
    let pattern = normalize(&params.name_matches);
    let wanted_categories = category_ids(&params.category, &categories);
    if !params.category.trim().is_empty() && wanted_categories.is_empty() {
        return json(Vec::<DanbooruTag>::new(), "");
    }
    let order = match params.order.as_str() {
        "name" => ListOrder::Name,
        "date" => ListOrder::Newest,
        _ => ListOrder::Count,
    };
    let filter = TagFilter {
        names: (!names.is_empty()).then_some(names.as_slice()),
        pattern: (!pattern.is_empty()).then_some(pattern.as_str()),
        categories: &wanted_categories,
        used_only: yes(&params.hide_empty),
    };
    let found = tags::filter(db, &filter, order, offset, limit).await?;
    let tags: Vec<DanbooruTag> = found.into_iter().map(DanbooruTag::from).collect();
    json(tags, &params.list.only)
}

#[derive(Debug, Default, Deserialize)]
struct AutocompleteParams {
    #[serde(rename = "search[query]", default)]
    query: String,
    #[serde(rename = "search[type]", default)]
    kind: String,
    #[serde(default)]
    limit: String,
}

#[derive(Debug, Serialize)]
struct Suggestion {
    #[serde(rename = "type")]
    kind: &'static str,
    label: String,
    value: String,
    category: i16,
    post_count: i32,
    antecedent: Option<String>,
}

/// Tags for the word being typed: the last one of a search
/// (`search[type]=tag_query`) or a single tag (`tag`).
async fn autocomplete(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<AutocompleteParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    if !matches!(params.kind.as_str(), "" | "tag_query" | "tag") {
        return json(Vec::<Suggestion>::new(), "");
    }
    let word = params.query.split_whitespace().last().unwrap_or_default();
    let prefix = normalize(word.trim_start_matches(['-', '~']));
    if prefix.is_empty() {
        return json(Vec::<Suggestion>::new(), "");
    }
    let limit = params.limit.parse::<i64>().unwrap_or(10).clamp(1, 20);
    let found = tags::autocomplete(state.reader(&current), &prefix, limit).await?;
    let suggestions: Vec<Suggestion> = found
        .into_iter()
        .map(|s| Suggestion {
            kind: if s.antecedent.is_some() {
                "tag-alias"
            } else {
                "tag"
            },
            label: s.name.replace('_', " "),
            value: s.name,
            category: s.category_id,
            post_count: s.post_count,
            antecedent: s.antecedent,
        })
        .collect();
    json(suggestions, "")
}

/// An alias or implication as Danbooru describes it.
#[derive(Debug, Serialize)]
struct DanbooruRelation {
    id: i32,
    antecedent_name: String,
    consequent_name: String,
    /// `active`, `pending` or `deleted` (rejected ones too).
    status: &'static str,
    reason: String,
    creator_id: Option<i64>,
    approver_id: Option<i64>,
    forum_topic_id: Option<i64>,
    forum_post_id: Option<i64>,
    created_at: String,
    updated_at: String,
}

impl From<Relation> for DanbooruRelation {
    fn from(r: Relation) -> Self {
        Self {
            id: r.id,
            status: match r.status {
                Status::Active => "active",
                Status::Pending => "pending",
                Status::Rejected | Status::Deleted => "deleted",
            },
            antecedent_name: r.antecedent,
            consequent_name: r.consequent,
            reason: r.reason,
            creator_id: r.creator_id,
            approver_id: None,
            forum_topic_id: None,
            forum_post_id: None,
            created_at: super::timestamp(r.created_at),
            updated_at: super::timestamp(r.updated_at),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RelationParams {
    #[serde(rename = "search[antecedent_name]", default)]
    antecedent: String,
    #[serde(rename = "search[consequent_name]", default)]
    consequent: String,
    #[serde(rename = "search[name_matches]", default)]
    either: String,
    #[serde(rename = "search[status]", default)]
    status: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn relations(
    state: &AppState,
    current: &CurrentUser,
    kind: Kind,
    params: RelationParams,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let (offset, limit) = window(&params.list, 1000)?;
    let status = match params.status.trim().to_lowercase().as_str() {
        "" | "any" => None,
        "active" => Some(Status::Active),
        "pending" => Some(Status::Pending),
        "deleted" => Some(Status::Deleted),
        // Danbooru's other states have no counterpart here.
        _ => return json(Vec::<DanbooruRelation>::new(), ""),
    };
    let name = |raw: &str| Some(normalize(raw)).filter(|n| !n.is_empty());
    let found = tag_relations::find(
        state.reader(current),
        kind,
        status,
        name(&params.antecedent).as_deref(),
        name(&params.consequent).as_deref(),
        name(&params.either).as_deref(),
        offset,
        limit,
    )
    .await?;
    let found: Vec<DanbooruRelation> = found.into_iter().map(DanbooruRelation::from).collect();
    json(found, &params.list.only)
}

async fn aliases(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<RelationParams>,
) -> Result<Response, AppError> {
    relations(&state, &current, Kind::Alias, params).await
}

async fn implications(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<RelationParams>,
) -> Result<Response, AppError> {
    relations(&state, &current, Kind::Implication, params).await
}

#[derive(Debug, Default, Deserialize)]
struct RelatedParams {
    #[serde(rename = "search[query]", default)]
    query: String,
    #[serde(rename = "search[category]", default)]
    category: String,
    #[serde(default)]
    limit: String,
}

#[derive(Debug, Serialize)]
struct RelatedTag {
    tag: DanbooruTag,
    cosine_similarity: f64,
    jaccard_similarity: f64,
    overlap_coefficient: f64,
    frequency: f64,
}

/// Posts looked at for related tags: the newest that match.
const RELATED_SAMPLE: u32 = 200;

/// Tags that often appear with a search, from its newest posts. The
/// similarities are estimates from that sample.
async fn related(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<RelatedParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let mut query = SearchQuery::parse(params.query.trim())
        .map_err(|e| AppError::Unprocessable(e.to_string()))?;
    query.limit = Some(RELATED_SAMPLE.min(state.config.search.max_per_page));
    let plan = Plan::resolve(db, &query, &visibility(&current), &state.config.search)
        .await
        .map_err(super::posts::search_error)?;
    let ids = plan
        .ids(db, PageRef::default())
        .await
        .map_err(super::posts::search_error)?;
    let limit = params.limit.parse::<usize>().unwrap_or(25).clamp(1, 100);
    let categories = tags::categories(db).await?;
    let wanted = category_ids(&params.category, &categories);
    let searched: Vec<&str> = query
        .all
        .iter()
        .chain(&query.any)
        .filter_map(|t| match t {
            TagTerm::Name(name) => Some(name.as_str()),
            TagTerm::Wildcard(_) => None,
        })
        .collect();

    let counts = tags::counts_among(db, &ids, 500).await?;
    let found: HashMap<i32, Tag> =
        tags::by_ids(db, &counts.iter().map(|(id, _)| *id).collect::<Vec<_>>())
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect();
    let sample = ids.len() as f64;
    let related: Vec<RelatedTag> = counts
        .into_iter()
        .filter_map(|(id, n)| found.get(&id).cloned().map(|t| (t, n as f64)))
        .filter(|(t, _)| !searched.contains(&t.name.as_str()))
        .filter(|(t, _)| wanted.is_empty() || wanted.contains(&t.category_id))
        .take(limit)
        .map(|(tag, n)| {
            let total = f64::from(tag.post_count.max(1));
            RelatedTag {
                cosine_similarity: n / (sample * total).sqrt(),
                jaccard_similarity: n / (sample + total - n).max(1.0),
                overlap_coefficient: n / sample.min(total).max(1.0),
                frequency: n / sample.max(1.0),
                tag: tag.into(),
            }
        })
        .collect();
    let tag = match searched.as_slice() {
        [single] => tags::by_name(db, single).await?.map(DanbooruTag::from),
        _ => None,
    };
    json(
        serde_json::json!({
            "query": params.query,
            "post_count": ids.len(),
            "tag": tag,
            "related_tags": related,
            "wiki_page_tags": [],
            "other_wikis": [],
        }),
        "",
    )
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use serde_json::{Value, json};
    use sqlx::PgPool;

    use crate::danbooru::test_support::{app, upload};
    use crate::test_support::session_for;

    async fn get(app: &crate::test_support::TestApp, path: &str) -> Value {
        let response = app.get(path, None).await;
        serde_json::from_str(&response.body).unwrap_or_else(|_| panic!("{}", response.body))
    }

    fn names(value: &Value, field: &str) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v[field].as_str().unwrap().to_owned())
            .collect()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn tags_and_autocomplete(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        upload(&app, &alice, 20, "long_hair cat artist:someone").await;
        upload(&app, &alice, 24, "long_hair cat_ears").await;
        sqlx::query("INSERT INTO tags (name) VALUES ('unused')")
            .execute(&pool)
            .await
            .unwrap();

        let all = get(&app, "/tags.json?search[order]=count&limit=2").await;
        assert_eq!(names(&all, "name")[0], "long_hair");
        assert_eq!(all[0]["post_count"], json!(2));
        assert_eq!(all[0]["words"], json!(["long", "hair"]));
        // Without a `*`, the whole name.
        let exact = get(&app, "/tags.json?search[name_matches]=cat").await;
        assert_eq!(names(&exact, "name"), ["cat"]);
        let pattern = get(
            &app,
            "/tags.json?search[name_matches]=cat*&search[order]=name",
        )
        .await;
        assert_eq!(names(&pattern, "name"), ["cat", "cat_ears"]);
        let listed = get(
            &app,
            "/tags.json?search[name_comma]=cat,long_hair,nope&search[order]=name",
        )
        .await;
        assert_eq!(names(&listed, "name"), ["cat", "long_hair"]);
        let artists = get(&app, "/tags.json?search[category]=1").await;
        assert_eq!(names(&artists, "name"), ["someone"]);
        let used = get(
            &app,
            "/tags.json?search[name_matches]=un*&search[hide_empty]=yes",
        )
        .await;
        assert_eq!(used, json!([]));

        let suggestions = get(
            &app,
            "/autocomplete.json?search[type]=tag_query&search[query]=cat+-lon",
        )
        .await;
        assert_eq!(names(&suggestions, "value"), ["long_hair"]);
        assert_eq!(suggestions[0]["label"], json!("long hair"));
        assert_eq!(suggestions[0]["type"], json!("tag"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn relations_and_related_tags(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        upload(&app, &alice, 20, "cat whiskers").await;
        upload(&app, &alice, 24, "cat whiskers paws").await;
        upload(&app, &alice, 28, "dog paws").await;
        sqlx::query(
            "INSERT INTO tag_relations (kind, antecedent_name, consequent_name, status)
             VALUES ('alias', 'kitty', 'cat', 'active'), ('implication', 'cat', 'animal', 'pending')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let aliases = get(&app, "/tag_aliases.json?search[consequent_name]=cat").await;
        assert_eq!(names(&aliases, "antecedent_name"), ["kitty"]);
        assert_eq!(aliases[0]["status"], json!("active"));
        let none = get(&app, "/tag_aliases.json?search[antecedent_name]=cat").await;
        assert_eq!(none, json!([]));
        let implied = get(
            &app,
            "/tag_implications.json?search[name_matches]=cat&search[status]=pending",
        )
        .await;
        assert_eq!(names(&implied, "consequent_name"), ["animal"]);

        let related = get(&app, "/related_tag.json?search[query]=cat").await;
        assert_eq!(related["tag"]["name"], json!("cat"));
        assert_eq!(related["post_count"], json!(2));
        let tags: Vec<&str> = related["related_tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["tag"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(tags, ["whiskers", "paws"]);
        assert_eq!(related["related_tags"][0]["frequency"], json!(1.0));
    }
}
