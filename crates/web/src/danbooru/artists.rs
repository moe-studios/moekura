//! `/artists.json`, `/artists/{id}.json`, `/artist_urls.json` and
//! `/artist_versions.json`, read-only.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::get;
use moekura_core::permissions::Permission;
use moekura_db::artists::{self, Artist, StoredUrl, VersionFilter};
use serde::{Deserialize, Serialize};

use super::tags::window;
use super::{ListParams, json, timestamp};
use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/artists", get(index))
        .route("/artists/{id}", get(show))
        .route("/artist_urls", get(urls))
        .route("/artist_versions", get(versions))
}

/// An artist as Danbooru describes it, with its URLs.
#[derive(Debug, Serialize)]
struct DanbooruArtist {
    id: i32,
    name: String,
    group_name: String,
    other_names: Vec<String>,
    is_banned: bool,
    is_deleted: bool,
    created_at: String,
    updated_at: String,
    urls: Vec<DanbooruUrl>,
}

#[derive(Debug, Serialize)]
struct DanbooruUrl {
    id: i64,
    artist_id: i32,
    url: String,
    normalized_url: String,
    is_active: bool,
    created_at: String,
    updated_at: String,
}

impl From<&StoredUrl> for DanbooruUrl {
    fn from(u: &StoredUrl) -> Self {
        Self {
            id: u.id,
            artist_id: u.artist_id,
            url: u.url.clone(),
            // Danbooru's form: a scheme and a trailing slash.
            normalized_url: format!("http://{}/", u.normalized_url),
            is_active: u.is_active,
            created_at: timestamp(u.created_at),
            updated_at: timestamp(u.created_at),
        }
    }
}

fn danbooru_artist(artist: Artist, urls: &[StoredUrl]) -> DanbooruArtist {
    DanbooruArtist {
        id: artist.id,
        urls: urls
            .iter()
            .filter(|u| u.artist_id == artist.id)
            .map(DanbooruUrl::from)
            .collect(),
        name: artist.name,
        group_name: artist.group_name,
        other_names: artist.other_names,
        is_banned: artist.is_banned,
        is_deleted: artist.is_deleted,
        created_at: timestamp(artist.created_at),
        updated_at: timestamp(artist.updated_at),
    }
}

fn flag(value: &str) -> Option<bool> {
    match value.trim() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

#[derive(Debug, Default, Deserialize)]
struct ArtistParams {
    #[serde(rename = "search[name]", default)]
    name: String,
    #[serde(rename = "search[any_name_matches]", default)]
    any_name_matches: String,
    #[serde(rename = "search[any_other_name_like]", default)]
    any_other_name_like: String,
    #[serde(rename = "search[url_matches]", default)]
    url_matches: String,
    #[serde(rename = "search[is_banned]", default)]
    is_banned: String,
    #[serde(rename = "search[is_deleted]", default)]
    is_deleted: String,
    #[serde(rename = "search[id]", default)]
    id: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn index(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<ArtistParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let (offset, limit) = window(&params.list, 1000)?;
    let name = [
        &params.name,
        &params.any_name_matches,
        &params.any_other_name_like,
    ]
    .into_iter()
    .map(|n| n.trim())
    .find(|n| !n.is_empty())
    .unwrap_or_default();
    // `search[name]` is the whole name, unless it has wildcards; the
    // others match the start of any name.
    let exact = (!params.name.trim().is_empty() && !name.contains('*'))
        .then(|| moekura_core::tags::normalize(name));
    let ids: Option<Vec<i32>> = match params.id.trim() {
        "" => None,
        list => Some(
            list.split(',')
                .filter_map(|i| i.trim().parse().ok())
                .collect(),
        ),
    };
    let deleted = flag(&params.is_deleted);
    let filter = artists::Filter {
        name: exact.as_deref().unwrap_or(name),
        url: &params.url_matches,
        banned: flag(&params.is_banned),
        with_deleted: deleted.unwrap_or(false),
        ids: ids.as_deref(),
    };
    let mut found = artists::list(db, &filter, offset, limit).await?;
    if deleted == Some(true) {
        found.retain(|a| a.is_deleted);
    }
    if let Some(exact) = &exact {
        found.retain(|a| &a.name == exact);
    }
    let ids: Vec<i32> = found.iter().map(|a| a.id).collect();
    let urls = artists::urls(db, &ids).await?;
    let list: Vec<DanbooruArtist> = found
        .into_iter()
        .map(|a| danbooru_artist(a, &urls))
        .collect();
    json(list, &params.list.only)
}

async fn show(
    State(state): State<AppState>,
    current: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let id: i32 = id.parse().map_err(|_| AppError::NotFound)?;
    let artist = artists::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    let urls = artists::urls(db, &[id]).await?;
    json(danbooru_artist(artist, &urls), "")
}

#[derive(Debug, Default, Deserialize)]
struct UrlParams {
    #[serde(rename = "search[artist_id]", default)]
    artist_id: String,
    #[serde(rename = "search[url_matches]", default)]
    url_matches: String,
    #[serde(rename = "search[is_active]", default)]
    is_active: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn urls(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<UrlParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let (offset, limit) = window(&params.list, 1000)?;
    let ids: Option<Vec<i32>> = match params.artist_id.trim() {
        "" => None,
        list => Some(
            list.split(',')
                .filter_map(|i| i.trim().parse().ok())
                .collect(),
        ),
    };
    let filter = artists::Filter {
        url: &params.url_matches,
        ids: ids.as_deref(),
        ..artists::Filter::default()
    };
    // URLs of the matching artists, a page of artists at a time.
    let found = artists::list(db, &filter, offset, limit).await?;
    let artist_ids: Vec<i32> = found.iter().map(|a| a.id).collect();
    let active = flag(&params.is_active);
    let list: Vec<DanbooruUrl> = artists::urls(db, &artist_ids)
        .await?
        .iter()
        .filter(|u| active.is_none_or(|a| u.is_active == a))
        .map(DanbooruUrl::from)
        .collect();
    json(list, &params.list.only)
}

#[derive(Debug, Serialize)]
struct DanbooruArtistVersion {
    id: i64,
    artist_id: i32,
    name: String,
    updater_id: Option<i64>,
    group_name: String,
    other_names: Vec<String>,
    urls: Vec<String>,
    is_banned: bool,
    is_deleted: bool,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Default, Deserialize)]
struct VersionParams {
    #[serde(rename = "search[artist_id]", default)]
    artist_id: String,
    #[serde(rename = "search[updater_id]", default)]
    updater_id: String,
    #[serde(rename = "search[updater_name]", default)]
    updater_name: String,
    #[serde(flatten)]
    list: ListParams,
}

async fn versions(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(params): Query<VersionParams>,
) -> Result<Response, AppError> {
    current.require(Permission::ViewPosts)?;
    let db = state.reader(&current);
    let (offset, limit) = window(&params.list, 1000)?;
    // Something that doesn't parse or exist matches nothing.
    let artist_id = match params.artist_id.trim() {
        "" => None,
        id => Some(id.parse().unwrap_or(-1)),
    };
    let updater_id = match (params.updater_id.trim(), params.updater_name.trim()) {
        ("", "") => None,
        ("", name) => Some(
            moekura_db::users::by_name(db, name)
                .await?
                .map_or(-1, |u| u.id),
        ),
        (id, _) => Some(id.parse().unwrap_or(-1)),
    };
    let filter = VersionFilter {
        artist_id,
        updater_id,
        before: None,
        offset,
    };
    let list: Vec<DanbooruArtistVersion> = artists::versions(db, filter, limit)
        .await?
        .into_iter()
        .map(|v| {
            let at = timestamp(v.created_at);
            DanbooruArtistVersion {
                id: v.id,
                artist_id: v.artist_id,
                name: v.name,
                updater_id: v.updater_id,
                group_name: v.group_name,
                other_names: v.other_names,
                urls: v.urls,
                is_banned: v.is_banned,
                is_deleted: v.is_deleted,
                updated_at: at.clone(),
                created_at: at,
            }
        })
        .collect();
    json(list, &params.list.only)
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use serde_json::Value;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn artists_urls_and_versions(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            crate::artists::routes().merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        app.post_form(
            "/artists",
            Some(&alice),
            &[],
            "name=cat_artist&other_names=nekoart&urls=https%3A%2F%2Ftwitter.com%2Fcatart",
        )
        .await;
        app.post_form("/artists", Some(&alice), &[], "name=dog_artist")
            .await;
        let parse = |body: &str| serde_json::from_str::<Value>(body).unwrap();

        let found = parse(
            &app.get("/artists.json?search[name]=cat_artist", None)
                .await
                .body,
        );
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(found[0]["other_names"][0], "nekoart");
        assert_eq!(found[0]["urls"][0]["url"], "https://twitter.com/catart");
        let id = found[0]["id"].as_i64().unwrap();
        let by_url = parse(
            &app.get(
                "/artists.json?search[url_matches]=https%3A%2F%2Ftwitter.com%2Fcatart%2Fstatus%2F2",
                None,
            )
            .await
            .body,
        );
        assert_eq!(by_url[0]["id"].as_i64(), Some(id));
        let other = parse(
            &app.get("/artists.json?search[any_name_matches]=neko*", None)
                .await
                .body,
        );
        assert_eq!(other[0]["id"].as_i64(), Some(id));
        assert_eq!(
            parse(&app.get("/artists.json", None).await.body)
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let one = parse(&app.get(&format!("/artists/{id}.json"), None).await.body);
        assert_eq!(one["name"], "cat_artist");
        let urls = parse(
            &app.get(&format!("/artist_urls.json?search[artist_id]={id}"), None)
                .await
                .body,
        );
        assert_eq!(urls[0]["normalized_url"], "http://twitter.com/catart/");
        let versions = parse(
            &app.get("/artist_versions.json?search[updater_name]=alice", None)
                .await
                .body,
        );
        assert_eq!(versions.as_array().unwrap().len(), 2);
        assert_eq!(versions[0]["name"], "dog_artist");
    }
}
