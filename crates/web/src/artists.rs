//! Artist entries: the other names, group and URLs of an artist tag, with
//! history; finding an artist by one of their URLs; and banning artists.

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::http::header::ACCEPT;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::artists::{ArtistUrl, GROUP_MAX_LEN};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_core::tags::TagName;
use moekura_db::artists::{self, Artist, Contents, SaveError, VersionFilter};
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::{tags, wiki};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::AppState;
use crate::auth::{CurrentUser, RequestInfo};
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::tags::{MAX_PAGE, PAGE_SIZE};
use crate::templates::{search_url, url_value};

/// Posts shown on an artist's page.
const POSTS_SHOWN: u32 = 12;

/// Versions per page of history.
const VERSIONS_PAGE: i64 = 50;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/artists", get(index).post(create))
        .route("/artists/new", get(new_form))
        .route("/artists/show_or_new", get(show_or_new))
        .route("/artists/finder", get(finder))
        .route("/artists/{id}", get(show).post(edit))
        .route("/artists/{id}/edit", get(edit_form))
        .route("/artists/{id}/history", get(history))
        .route("/artists/{id}/{action}", post(set_flag))
        .route("/artist_versions", get(recent_versions))
}

pub(crate) fn artist_url(id: i32) -> String {
    format!("/artists/{id}")
}

/// How a tag name reads: `some_artist` as "some artist".
fn display(name: &str) -> String {
    name.replace('_', " ")
}

/// Whether `current` sees deleted artists.
pub(crate) fn sees_deleted(current: &CurrentUser) -> bool {
    current.can(Permission::EditWiki) || current.can(Permission::ViewDeleted)
}

async fn visible_artist(db: &PgPool, current: &CurrentUser, id: i32) -> Result<Artist, AppError> {
    current.require(Permission::ViewPosts)?;
    let artist = artists::by_id(db, id).await?.ok_or(AppError::NotFound)?;
    if artist.is_deleted && !sees_deleted(current) {
        return Err(AppError::NotFound);
    }
    Ok(artist)
}

fn summary_context(artist: &Artist, urls: &[&artists::StoredUrl]) -> Value {
    context! {
        id => artist.id,
        name => artist.name,
        title => display(&artist.name),
        url => url_value(&artist_url(artist.id)),
        other_names => artist.other_names.iter().map(|n| display(n)).collect::<Vec<_>>(),
        group => artist.group_name,
        banned => artist.is_banned,
        deleted => artist.is_deleted,
        urls => urls.iter().map(|u| url_context(u)).collect::<Vec<_>>(),
        updater => artist.updater_name,
        date => crate::dates::day(artist.updated_at),
    }
}

fn url_context(url: &artists::StoredUrl) -> Value {
    context! {
        url => url.url,
        // Safe to link: only http(s) addresses are stored.
        href => url_value(&url.url),
        active => url.is_active,
    }
}

// ---- checking input ---------------------------------------------------------

/// An artist's fields as typed, from a form or the API.
pub(crate) struct ArtistInput<'a> {
    pub name: &'a str,
    pub group_name: &'a str,
    /// Separated by spaces.
    pub other_names: &'a str,
    /// One per line; `-` in front for inactive ones.
    pub urls: &'a str,
}

/// Checks `input` and turns it into what's saved.
pub(crate) fn contents(
    input: &ArtistInput<'_>,
    is_banned: bool,
    is_deleted: bool,
) -> Result<Contents, AppError> {
    let name = TagName::parse(input.name)
        .map_err(|e| AppError::Unprocessable(format!("The name {e}.")))?;
    let group_name = input.group_name.trim().to_owned();
    if group_name.chars().count() > GROUP_MAX_LEN {
        return Err(AppError::Unprocessable(format!(
            "The group name can be at most {GROUP_MAX_LEN} characters long."
        )));
    }
    let other_names = moekura_core::wiki::parse_other_names(input.other_names)
        .map_err(|e| AppError::Unprocessable(e.to_string()))?;
    let urls =
        ArtistUrl::parse_list(input.urls).map_err(|e| AppError::Unprocessable(e.to_string()))?;
    Ok(Contents {
        name: name.as_str().to_owned(),
        group_name,
        other_names,
        urls,
        is_banned,
        is_deleted,
    })
}

fn save_error(error: SaveError) -> AppError {
    match error {
        SaveError::Conflict { .. } => AppError::Conflict(
            "Someone else changed this artist while you were editing. Check their changes, \
             then save again."
                .into(),
        ),
        SaveError::NameTaken => {
            AppError::Unprocessable("There's already an artist entry with that name.".into())
        }
        SaveError::BanLocked => AppError::Unprocessable(
            "This artist is banned: only those who manage tags can rename, delete or restore \
             the entry."
                .into(),
        ),
        SaveError::Db(e) => e.into(),
    }
}

/// Makes sure the artist's tag exists: new, or unused so far, it becomes
/// an artist tag; a used tag keeps its category.
async fn claim_tag(state: &AppState, current: &CurrentUser, name: &str) -> Result<(), AppError> {
    let db = state.db.primary();
    let category_id = tags::categories(db)
        .await?
        .into_iter()
        .find(|c| c.name == "artist")
        .map(|c| c.id);
    let mut tx = db.begin().await?;
    moekura_db::post_versions::attribute(&mut tx, current.user.as_ref().map(|u| u.id), None)
        .await?;
    let wanted = [tags::WantedTag { name, category_id }];
    tags::ensure(&mut tx, &wanted, false).await?;
    tx.commit().await?;
    Ok(())
}

/// Creates an artist entry as `current`; returns its id.
pub(crate) async fn create_artist(
    state: &AppState,
    current: &CurrentUser,
    contents: &Contents,
) -> Result<i32, AppError> {
    current.require(Permission::EditWiki)?;
    let id = artists::create(
        state.db.primary(),
        contents,
        current.user.as_ref().map(|u| u.id),
    )
    .await
    .map_err(save_error)?;
    claim_tag(state, current, &contents.name).await?;
    Ok(id)
}

/// Saves artist `id` as `current`; see [`artists::save`] for `base`.
/// Changing a ban, a banned entry's name or whether it's deleted takes
/// [`Permission::ManageTags`].
pub(crate) async fn save_artist(
    state: &AppState,
    current: &CurrentUser,
    id: i32,
    contents: &Contents,
    base: Option<i32>,
) -> Result<i32, AppError> {
    current.require(Permission::EditWiki)?;
    let version = artists::save(
        state.db.primary(),
        id,
        contents,
        current.user.as_ref().map(|u| u.id),
        base,
        current.can(Permission::ManageTags),
    )
    .await
    .map_err(save_error)?;
    claim_tag(state, current, &contents.name).await?;
    Ok(version)
}

/// The tag an entry's ban applies to: its name, while it's banned and
/// not deleted.
fn banned_tag(name: &str, is_banned: bool, is_deleted: bool) -> Option<&str> {
    (is_banned && !is_deleted).then_some(name)
}

/// Logs a ban that renaming, deleting or restoring a banned entry moved
/// or lifted (bans and unbans are logged as such): `was` and `now` are
/// the banned tag before and after ([`banned_tag`]).
async fn record_ban_moved(
    db: &PgPool,
    current: &CurrentUser,
    id: i32,
    was: Option<&str>,
    now: Option<&str>,
) -> Result<(), AppError> {
    let (kind, details) = match (was, now) {
        (Some(old), Some(new)) if old != new => (
            ActionKind::ArtistBan,
            serde_json::json!({ "artist_id": id, "name": new, "previous_name": old }),
        ),
        (Some(old), None) => (
            ActionKind::ArtistUnban,
            serde_json::json!({ "artist_id": id, "name": old, "deleted": true }),
        ),
        (None, Some(new)) => (
            ActionKind::ArtistBan,
            serde_json::json!({ "artist_id": id, "name": new, "deleted": false }),
        ),
        _ => return Ok(()),
    };
    let actor = current.user.as_ref().map(|u| u.id);
    mod_actions::record(db, NewAction::new(actor, kind).details(details)).await?;
    Ok(())
}

/// Refuses tags `new` (a post's, `old` before the change) that add a
/// banned artist's tag, when the site refuses those and `current` doesn't
/// approve posts.
pub(crate) async fn refuse_banned(
    state: &AppState,
    db: impl sqlx::PgExecutor<'_>,
    current: &CurrentUser,
    new: &[i32],
    old: &[i32],
) -> Result<(), String> {
    if !state.site.get().settings.banned_artists.refuse_uploads
        || current.can(Permission::ApprovePosts)
    {
        return Ok(());
    }
    let added: Vec<i32> = new.iter().filter(|t| !old.contains(t)).copied().collect();
    if added.is_empty() {
        return Ok(());
    }
    let banned = artists::banned_among_tags(db, &added)
        .await
        .map_err(|e| e.to_string())?;
    match banned.as_slice() {
        [] => Ok(()),
        names => Err(format!(
            "Works by {} can't be posted here: the artist is banned.",
            names
                .iter()
                .map(|n| display(n))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

// ---- list -------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct IndexQuery {
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    /// `yes` or `no`.
    #[serde(default)]
    banned: String,
    page: Option<i64>,
}

async fn index(page: Page, Query(query): Query<IndexQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let number = query.page.unwrap_or(1).clamp(1, MAX_PAGE);
    let name = query.name.split_whitespace().collect::<Vec<_>>().join("_");
    let filter = artists::Filter {
        name: &name,
        url: &query.url,
        banned: match query.banned.as_str() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        },
        with_deleted: false,
        ids: None,
    };
    let mut found = artists::list(db, &filter, (number - 1) * PAGE_SIZE, PAGE_SIZE + 1).await?;
    let has_next = found.len() > PAGE_SIZE as usize && number < MAX_PAGE;
    found.truncate(PAGE_SIZE as usize);
    let ids: Vec<i32> = found.iter().map(|a| a.id).collect();
    let urls = artists::urls(db, &ids).await?;
    let rows: Vec<Value> = found
        .iter()
        .map(|a| {
            let mine: Vec<_> = urls.iter().filter(|u| u.artist_id == a.id).collect();
            summary_context(a, &mine)
        })
        .collect();
    let list_url = |n: i64| {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("name", &query.name)
            .append_pair("url", &query.url)
            .append_pair("banned", &query.banned)
            .append_pair("page", &n.to_string())
            .finish();
        url_value(&format!("/artists?{q}"))
    };
    Ok(page.render(
        "artists.html",
        context! {
            artists => rows,
            query => context! { name => query.name, url => query.url, banned => query.banned },
            can_create => page.current.can(Permission::EditWiki),
            previous_url => (number > 1).then(|| list_url(number - 1)),
            next_url => has_next.then(|| list_url(number + 1)),
        },
    ))
}

#[derive(Debug, Default, Deserialize)]
struct NameQuery {
    #[serde(default)]
    name: String,
    /// For a new entry: its URLs, one per line.
    #[serde(default)]
    urls: String,
    /// For a new entry: its other names.
    #[serde(default)]
    other_names: String,
}

/// The entry for an artist tag, or the form to start one.
async fn show_or_new(page: Page, Query(query): Query<NameQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let name = TagName::parse(&query.name).map_err(|_| AppError::NotFound)?;
    let db = page.state().reader(&page.current);
    match artists::by_name(db, name.as_str()).await? {
        Some(artist) => Ok(Redirect::to(&artist_url(artist.id)).into_response()),
        None => {
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("name", name.as_str())
                .finish();
            Ok(Redirect::to(&format!("/artists/new?{q}")).into_response())
        }
    }
}

// ---- finding by URL ---------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct FinderQuery {
    #[serde(default)]
    url: String,
}

/// An artist found from a URL, for the upload form's script.
#[derive(Debug, Serialize)]
struct Found {
    id: i32,
    name: String,
    url: String,
}

/// An artist the source names who has no entry yet.
#[derive(Debug, Serialize)]
pub(crate) struct Unknown {
    /// Their name there.
    pub name: String,
    /// A tag name to suggest: their account's.
    pub tag: Option<String>,
    /// The form to start their entry, filled in.
    pub new_url: String,
}

/// The artist a source names, and the form starting their entry, filled
/// in with their account (as the tag), names and profiles; `None` when
/// it names nobody.
pub(crate) fn unknown_artist(info: &crate::sources::SourceInfo) -> Option<Unknown> {
    let name = info
        .artist_name
        .clone()
        .or_else(|| info.artist_account.clone())?;
    let tag = info
        .artist_account
        .as_deref()
        .and_then(|a| TagName::parse(a).ok())
        .map(TagName::into_string);
    let mut other_names: Vec<String> =
        [info.artist_name.as_deref(), info.artist_account.as_deref()]
            .into_iter()
            .flatten()
            .map(moekura_core::wiki::normalize_other_name)
            .filter(|n| Some(n) != tag.as_ref())
            .collect();
    other_names.dedup();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("name", tag.as_deref().unwrap_or_default())
        .append_pair("other_names", &other_names.join(" "))
        .append_pair("urls", &info.profile_urls.join("\n"))
        .finish();
    Some(Unknown {
        name,
        tag,
        new_url: format!("/artists/new?{query}"),
    })
}

#[derive(Debug, Serialize)]
struct FinderAnswer {
    artists: Vec<Found>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unknown: Option<Unknown>,
}

/// The artists a URL (a profile, a post, or a work's page on a site the
/// source strategies read) belongs to, and, when the source names an
/// artist without an entry, how to start one.
async fn find_for_url(
    state: &AppState,
    current: &CurrentUser,
    request: &RequestInfo,
    db: &PgPool,
    url: &str,
) -> Result<(Vec<Artist>, Option<Unknown>), AppError> {
    let url = url.trim();
    if url.is_empty() {
        return Ok((Vec::new(), None));
    }
    let mut found = artists::find_by_url(db, url).await?;
    let mut unknown = None;
    if let Some(info) =
        crate::sources::lookup_for_page(state, current, request.ip, url, false).await?
    {
        for artist in crate::sources::artists_for(db, &info).await? {
            if !found.iter().any(|a| a.id == artist.id) {
                found.push(artist);
            }
        }
        if found.is_empty() {
            unknown = unknown_artist(&info);
        }
    }
    Ok((found, unknown))
}

/// The artists a URL belongs to, as a page or, for scripts asking for
/// JSON, a list.
async fn finder(
    page: Page,
    request: RequestInfo,
    headers: HeaderMap,
    Query(query): Query<FinderQuery>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.reader(&page.current);
    let (found, unknown) = find_for_url(state, &page.current, &request, db, &query.url).await?;
    let wants_json = headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"));
    if wants_json {
        let artists: Vec<Found> = found
            .iter()
            .map(|a| Found {
                id: a.id,
                name: a.name.clone(),
                url: artist_url(a.id),
            })
            .collect();
        return Ok(Json(FinderAnswer { artists, unknown }).into_response());
    }
    let ids: Vec<i32> = found.iter().map(|a| a.id).collect();
    let urls = artists::urls(db, &ids).await?;
    Ok(page.render(
        "artist_finder.html",
        context! {
            query => query.url,
            searched => !query.url.trim().is_empty(),
            artists => found.iter().map(|a| {
                let mine: Vec<_> = urls.iter().filter(|u| u.artist_id == a.id).collect();
                summary_context(a, &mine)
            }).collect::<Vec<_>>(),
            unknown => unknown.map(|u| context! {
                name => u.name,
                new_url => url_value(&u.new_url),
            }),
            can_create => page.current.can(Permission::EditWiki),
        },
    ))
}

// ---- one artist -------------------------------------------------------------

async fn show(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    let db = page.state().reader(&page.current);
    let artist = visible_artist(db, &page.current, id).await?;
    let urls = artists::urls(db, &[id]).await?;
    let tag = tags::by_name(db, &artist.name).await?;
    let wiki_page = wiki::by_title(db, &artist.name).await?;
    let excerpt = wiki_page
        .as_ref()
        .map(|p| markup::excerpt(&p.body))
        .filter(|e| !e.is_empty());
    let posts = crate::posts::preview(&page, &artist.name, POSTS_SHOWN).await?;
    let mine: Vec<_> = urls.iter().collect();
    Ok(page.render(
        "artist.html",
        context! {
            artist => summary_context(&artist, &mine),
            version => artist.version,
            post_count => tag.map(|t| t.post_count),
            search_url => Value::from_safe_string(search_url(&artist.name)),
            wiki => context! {
                url => Value::from_safe_string(markup::wiki_url(&artist.name)),
                exists => wiki_page.is_some(),
                html => excerpt.map(|e| Value::from_safe_string(markup::render(&e))),
            },
            posts => posts,
            can_edit => page.current.can(Permission::EditWiki),
            can_ban => page.current.can(Permission::ManageTags),
        },
    ))
}

fn form_context(
    artist: Option<&Artist>,
    current: &CurrentUser,
    input: &ArtistInput<'_>,
    base: i32,
    error: Option<String>,
) -> Value {
    context! {
        artist => artist.map(|a| context! {
            title => display(&a.name),
            url => url_value(&artist_url(a.id)),
        }),
        action => artist.map_or_else(|| "/artists".to_owned(), |a| artist_url(a.id)),
        // A banned entry's name is part of its ban.
        name_locked => artist.is_some_and(|a| a.is_banned) && !current.can(Permission::ManageTags),
        name => input.name,
        group_name => input.group_name,
        other_names => input.other_names,
        urls => input.urls,
        base => base,
        error => error,
    }
}

async fn new_form(page: Page, Query(query): Query<NameQuery>) -> Result<Response, AppError> {
    page.current.require(Permission::EditWiki)?;
    let input = ArtistInput {
        name: query.name.trim(),
        group_name: "",
        other_names: query.other_names.trim(),
        urls: query.urls.trim(),
    };
    Ok(page.render(
        "artist_edit.html",
        form_context(None, &page.current, &input, 0, None),
    ))
}

#[derive(Debug, Deserialize)]
struct ArtistForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    group_name: String,
    #[serde(default)]
    other_names: String,
    #[serde(default)]
    urls: String,
    /// The version the form was loaded with.
    #[serde(default)]
    base: i32,
}

impl ArtistForm {
    fn input(&self) -> ArtistInput<'_> {
        ArtistInput {
            name: &self.name,
            group_name: &self.group_name,
            other_names: &self.other_names,
            urls: &self.urls,
        }
    }
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<ArtistForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditWiki)?;
    let state = page.state();
    let saved = match contents(&form.input(), false, false) {
        Ok(contents) => create_artist(state, &page.current, &contents).await,
        Err(error) => Err(error),
    };
    match saved {
        Ok(id) => {
            Ok((flash::set(jar, Flash::Saved), Redirect::to(&artist_url(id))).into_response())
        }
        Err(error @ (AppError::Unprocessable(_) | AppError::Conflict(_))) => Ok(page
            .render_with_status(
                error.status(),
                "artist_edit.html",
                form_context(
                    None,
                    &page.current,
                    &form.input(),
                    0,
                    Some(error.public_message().to_owned()),
                ),
            )),
        Err(error) => Err(error),
    }
}

async fn edit_form(page: Page, Path(id): Path<i32>) -> Result<Response, AppError> {
    page.current.require(Permission::EditWiki)?;
    let db = page.state().db.primary();
    let artist = visible_artist(db, &page.current, id).await?;
    let saved = artists::contents(db, id).await?.ok_or(AppError::NotFound)?;
    let other_names = saved.other_names.join(" ");
    let urls = ArtistUrl::to_list(&saved.urls);
    let input = ArtistInput {
        name: &saved.name,
        group_name: &saved.group_name,
        other_names: &other_names,
        urls: &urls,
    };
    Ok(page.render(
        "artist_edit.html",
        form_context(Some(&artist), &page.current, &input, artist.version, None),
    ))
}

async fn edit(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i32>,
    Form(form): Form<ArtistForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::EditWiki)?;
    let state = page.state();
    let db = state.db.primary();
    let artist = visible_artist(db, &page.current, id).await?;
    let saved = match contents(&form.input(), artist.is_banned, artist.is_deleted) {
        Ok(contents) => save_artist(state, &page.current, id, &contents, Some(form.base))
            .await
            .map(|_| contents),
        Err(error) => Err(error),
    };
    match saved {
        Ok(contents) => {
            record_ban_moved(
                db,
                &page.current,
                id,
                banned_tag(&artist.name, artist.is_banned, artist.is_deleted),
                banned_tag(&contents.name, contents.is_banned, contents.is_deleted),
            )
            .await?;
            Ok((flash::set(jar, Flash::Saved), Redirect::to(&artist_url(id))).into_response())
        }
        Err(error @ (AppError::Unprocessable(_) | AppError::Conflict(_))) => Ok(page
            .render_with_status(
                error.status(),
                "artist_edit.html",
                form_context(
                    Some(&artist),
                    &page.current,
                    &form.input(),
                    form.base,
                    Some(error.public_message().to_owned()),
                ),
            )),
        Err(error) => Err(error),
    }
}

/// Bans, unbans, deletes or restores artist `id`.
pub(crate) async fn set_status(
    state: &AppState,
    current: &CurrentUser,
    id: i32,
    action: &str,
) -> Result<(), AppError> {
    let db = state.db.primary();
    let before = artists::contents(db, id).await?.ok_or(AppError::NotFound)?;
    let mut contents = before.clone();
    let kind = match action {
        "ban" | "unban" => {
            current.require(Permission::ManageTags)?;
            contents.is_banned = action == "ban";
            Some(if contents.is_banned {
                ActionKind::ArtistBan
            } else {
                ActionKind::ArtistUnban
            })
        }
        "delete" | "undelete" => {
            current.require(Permission::EditWiki)?;
            // Deleting a banned entry lifts its ban; restoring it bans again.
            if contents.is_banned {
                current.require(Permission::ManageTags)?;
            }
            contents.is_deleted = action == "delete";
            None
        }
        _ => return Err(AppError::NotFound),
    };
    let actor = current.user.as_ref().map(|u| u.id);
    artists::save(
        db,
        id,
        &contents,
        actor,
        None,
        current.can(Permission::ManageTags),
    )
    .await
    .map_err(save_error)?;
    if let Some(kind) = kind {
        mod_actions::record(
            db,
            NewAction::new(actor, kind)
                .details(serde_json::json!({ "artist_id": id, "name": contents.name })),
        )
        .await?;
    } else {
        record_ban_moved(
            db,
            current,
            id,
            banned_tag(&before.name, before.is_banned, before.is_deleted),
            banned_tag(&contents.name, contents.is_banned, contents.is_deleted),
        )
        .await?;
    }
    // This node sees the change at once; others when told.
    state.site.reload(db).await?;
    Ok(())
}

async fn set_flag(
    page: Page,
    jar: CookieJar,
    Path((id, action)): Path<(i32, String)>,
) -> Result<Response, AppError> {
    set_status(page.state(), &page.current, id, &action).await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&artist_url(id))).into_response())
}

// ---- history ----------------------------------------------------------------

/// What a version changed, for the history lists.
fn version_context(v: &artists::Version) -> Value {
    let first = v.previous_name.is_none();
    fn listed(now: &[String], before: Option<&Vec<String>>) -> (Vec<String>, Vec<String>) {
        let before = before.map(Vec::as_slice).unwrap_or_default();
        let added = now
            .iter()
            .filter(|n| !before.contains(n))
            .cloned()
            .collect();
        let removed = before
            .iter()
            .filter(|n| !now.contains(n))
            .cloned()
            .collect();
        (added, removed)
    }
    let (names_added, names_removed) = listed(&v.other_names, v.previous_other_names.as_ref());
    let (urls_added, urls_removed) = listed(&v.urls, v.previous_urls.as_ref());
    let changed = |now: &str, before: Option<&String>| {
        (first || before.is_some_and(|b| b != now)).then(|| now.to_owned())
    };
    let flag = |now: bool, before: Option<bool>| before.is_some_and(|b| b != now).then_some(now);
    context! {
        artist_id => v.artist_id,
        title => display(&v.name),
        version => v.version,
        date => crate::dates::day(v.created_at),
        time => crate::dates::clock(v.created_at),
        updater => v.updater_name,
        created => first,
        name => changed(&v.name, v.previous_name.as_ref()),
        group => changed(&v.group_name, v.previous_group_name.as_ref()).filter(|g| !g.is_empty() || !first),
        names_added => names_added,
        names_removed => names_removed,
        urls_added => urls_added,
        urls_removed => urls_removed,
        banned => flag(v.is_banned, v.previous_is_banned),
        deleted => flag(v.is_deleted, v.previous_is_deleted),
    }
}

#[derive(Debug, Default, Deserialize)]
struct HistoryQuery {
    #[serde(default)]
    user: String,
    before: Option<i64>,
}

async fn render_versions(
    page: &Page,
    artist: Option<&Artist>,
    query: &HistoryQuery,
) -> Result<Response, AppError> {
    let db = page.state().reader(&page.current);
    let updater_id = match query.user.trim() {
        "" => None,
        name => Some(
            moekura_db::users::by_name(db, name)
                .await?
                .map_or(-1, |u| u.id),
        ),
    };
    let filter = VersionFilter {
        artist_id: artist.map(|a| a.id),
        updater_id,
        before: query.before,
        offset: 0,
    };
    let found = artists::versions(db, filter, VERSIONS_PAGE + 1).await?;
    let more = found.len() > VERSIONS_PAGE as usize;
    // Deleted artists' changes stay out of the sitewide list for those
    // who can't see them.
    let hidden: Vec<i32> = if artist.is_none() && !sees_deleted(&page.current) {
        let ids: Vec<i32> = found.iter().map(|v| v.artist_id).collect();
        artists::by_ids(db, &ids)
            .await?
            .into_iter()
            .filter(|a| a.is_deleted)
            .map(|a| a.id)
            .collect()
    } else {
        Vec::new()
    };
    let shown: Vec<&artists::Version> = found
        .iter()
        .take(VERSIONS_PAGE as usize)
        .filter(|v| !hidden.contains(&v.artist_id))
        .collect();
    let path = artist.map_or_else(
        || "/artist_versions".to_owned(),
        |a| format!("{}/history", artist_url(a.id)),
    );
    let next_url = more
        .then(|| found.get(VERSIONS_PAGE as usize - 1))
        .flatten()
        .map(|last| {
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("user", &query.user)
                .append_pair("before", &last.id.to_string())
                .finish();
            url_value(&format!("{path}?{q}"))
        });
    Ok(page.render(
        "artist_versions.html",
        context! {
            artist => artist.map(|a| context! {
                title => display(&a.name),
                url => url_value(&artist_url(a.id)),
            }),
            path => path,
            query => context! { user => query.user },
            versions => shown.iter().map(|v| version_context(v)).collect::<Vec<_>>(),
            next_url => next_url,
        },
    ))
}

async fn history(
    page: Page,
    Path(id): Path<i32>,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, AppError> {
    let artist = visible_artist(page.state().reader(&page.current), &page.current, id).await?;
    render_versions(&page, Some(&artist), &query).await
}

async fn recent_versions(
    page: Page,
    Query(query): Query<HistoryQuery>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    render_versions(&page, None, &query).await
}

/// The artist entry for tag `name`, if there is one `current` may see,
/// for linking from wiki pages and searches.
pub(crate) async fn entry_for(
    db: &PgPool,
    current: &CurrentUser,
    name: &str,
) -> Result<Option<Value>, AppError> {
    Ok(artists::by_name(db, name)
        .await?
        .filter(|a| !a.is_deleted || sees_deleted(current))
        .map(|a| {
            context! {
                url => url_value(&artist_url(a.id)),
                banned => a.is_banned,
            }
        }))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        TestApp::new(
            test_state(pool).await,
            super::routes().merge(crate::danbooru::test_support::routes()),
        )
    }

    fn id_from(location: &str) -> i32 {
        location.trim_start_matches("/artists/").parse().unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn create_edit_find_and_history(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        assert_eq!(
            app.post_form("/artists", None, &[], "name=x").await.status,
            StatusCode::UNAUTHORIZED
        );
        let created = app
            .post_form(
                "/artists",
                Some(&alice),
                &[],
                "name=Cat+Artist&group_name=Cats&other_names=%E3%83%8D%E3%82%B3+nekoart\
                 &urls=https%3A%2F%2Ftwitter.com%2Fcatart%0D%0A-pixiv.net%2Fusers%2F9",
            )
            .await;
        assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
        let id = id_from(&created.location.unwrap());
        // The tag exists now, as an artist tag.
        let tag = moekura_db::tags::by_name(&pool, "cat_artist")
            .await
            .unwrap()
            .unwrap();
        let categories = moekura_db::tags::categories(&pool).await.unwrap();
        assert_eq!(
            categories
                .iter()
                .find(|c| c.id == tag.category_id)
                .unwrap()
                .name,
            "artist"
        );

        let shown = app.get(&format!("/artists/{id}"), None).await;
        assert_eq!(shown.status, StatusCode::OK);
        // Saved in its canonical form, with the site's icon.
        assert!(shown.body.contains("x.com&#x2f;catart"), "{}", shown.body);
        assert!(
            shown.body.contains(r#"<title>X</title>"#) && shown.body.contains("#twitter"),
            "{}",
            shown.body
        );
        assert!(shown.body.contains("ネコ"), "{}", shown.body);

        let dup = app
            .post_form("/artists", Some(&alice), &[], "name=cat_artist")
            .await;
        assert_eq!(dup.status, StatusCode::UNPROCESSABLE_ENTITY);

        let found = app
            .get_json(
                "/artists/finder?url=https%3A%2F%2Fx.com%2Fcatart%2Fstatus%2F1",
                None,
            )
            .await;
        assert!(
            found.body.contains("\"name\":\"cat_artist\""),
            "{}",
            found.body
        );
        assert_eq!(
            app.get("/artists/show_or_new?name=Cat_Artist", None)
                .await
                .location
                .as_deref(),
            Some(format!("/artists/{id}").as_str())
        );

        let edited = app
            .post_form(
                &format!("/artists/{id}"),
                Some(&alice),
                &[],
                "name=cat_artist&group_name=&other_names=nekoart&urls=twitter.com%2Fcatart&base=1",
            )
            .await;
        assert_eq!(edited.status, StatusCode::SEE_OTHER, "{}", edited.body);
        let stale = app
            .post_form(
                &format!("/artists/{id}"),
                Some(&alice),
                &[],
                "name=cat_artist&base=1",
            )
            .await;
        assert_eq!(stale.status, StatusCode::CONFLICT);
        let history = app.get(&format!("/artists/{id}/history"), None).await.body;
        assert!(history.contains("pixiv.net"), "{history}");
        let sitewide = app.get("/artist_versions?user=alice", None).await.body;
        assert!(sitewide.contains("cat artist"), "{sitewide}");
        let listed = app.get("/artists?name=neko*", None).await.body;
        assert!(listed.contains(&format!("/artists/{id}\"")), "{listed}");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn banned_artists_are_hidden_and_refused(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let boss = session_for(&pool, "boss", SystemRole::Admin).await;
        let post = crate::danbooru::test_support::upload(&app, &alice, 20, "bad_artist cat").await;
        let created = app
            .post_form("/artists", Some(&alice), &[], "name=bad_artist")
            .await;
        let id = id_from(&created.location.unwrap());
        assert_eq!(
            app.post(&format!("/artists/{id}/ban"), Some(&alice), &[])
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let banned = app
            .post(&format!("/artists/{id}/ban"), Some(&boss), &[])
            .await;
        assert_eq!(banned.status, StatusCode::SEE_OTHER);

        // Hidden from members and visitors, not from staff.
        let page = format!("/posts/{post}");
        assert_eq!(
            app.get(&page, Some(&alice)).await.status,
            StatusCode::NOT_FOUND
        );
        assert_eq!(app.get(&page, Some(&boss)).await.status, StatusCode::OK);
        let search = app.get("/posts?tags=cat", None).await.body;
        assert!(
            !search.contains(&format!("href=\"/posts/{post}")),
            "{search}"
        );

        // New uploads by the artist are refused.
        let fields = vec![
            ("rating", "s".to_owned()),
            ("tags", "bad_artist".to_owned()),
        ];
        let png = crate::test_support::fixture::png(30, 20);
        let refused = app
            .post_multipart("/upload", Some(&alice), &fields, Some(("a.png", &png)))
            .await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            refused.body
        );
        assert!(refused.body.contains("banned"), "{}", refused.body);

        app.post(&format!("/artists/{id}/unban"), Some(&boss), &[])
            .await;
        assert_eq!(app.get(&page, Some(&alice)).await.status, StatusCode::OK);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn only_tag_managers_move_or_lift_a_ban(pool: PgPool) {
        let app = app(&pool).await;
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let boss = session_for(&pool, "boss", SystemRole::Admin).await;
        let hidden =
            crate::danbooru::test_support::upload(&app, &alice, 20, "bad_artist cat").await;
        let shown = crate::danbooru::test_support::upload(&app, &alice, 30, "cat").await;
        let created = app
            .post_form("/artists", Some(&alice), &[], "name=bad_artist")
            .await;
        let id = id_from(&created.location.unwrap());
        app.post(&format!("/artists/{id}/ban"), Some(&boss), &[])
            .await;
        let visible = async |post: i64| {
            app.get(&format!("/posts/{post}"), Some(&alice))
                .await
                .status
                == StatusCode::OK
        };
        assert!(!visible(hidden).await && visible(shown).await);

        // A member can't move the ban onto another tag, nor lift it.
        let page = format!("/artists/{id}");
        let renamed = app
            .post_form(&page, Some(&alice), &[], "name=cat&base=2")
            .await;
        assert_eq!(
            renamed.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            renamed.body
        );
        assert!(renamed.body.contains("manage tags"), "{}", renamed.body);
        for action in ["delete", "undelete"] {
            assert_eq!(
                app.post(&format!("{page}/{action}"), Some(&alice), &[])
                    .await
                    .status,
                StatusCode::FORBIDDEN
            );
        }
        assert!(!visible(hidden).await && visible(shown).await);
        // The rest of the entry is still theirs to edit, the name read-only.
        let form = app.get(&format!("{page}/edit"), Some(&alice)).await.body;
        assert!(form.contains(" readonly>"), "{form}");
        assert!(
            !app.get(&page, Some(&alice))
                .await
                .body
                .contains("/delete\"")
        );
        let grouped = app
            .post_form(
                &page,
                Some(&alice),
                &[],
                "name=bad_artist&group_name=Bad&base=2",
            )
            .await;
        assert_eq!(grouped.status, StatusCode::SEE_OTHER, "{}", grouped.body);

        // Staff can, and the log says what happened to the ban.
        let moved = app
            .post_form(&page, Some(&boss), &[], "name=worse_artist&base=3")
            .await;
        assert_eq!(moved.status, StatusCode::SEE_OTHER, "{}", moved.body);
        app.post(&format!("{page}/delete"), Some(&boss), &[]).await;
        assert_eq!(
            app.post(&format!("{page}/undelete"), Some(&alice), &[])
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let log: Vec<(String, serde_json::Value)> = sqlx::query_as(
            "SELECT action, details FROM mod_actions WHERE action LIKE 'artist.%' ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let log: Vec<(&str, serde_json::Value)> = log
            .iter()
            .map(|(action, details)| (action.as_str(), details.clone()))
            .collect();
        assert_eq!(
            log,
            [
                (
                    "artist.ban",
                    serde_json::json!({ "artist_id": id, "name": "bad_artist" })
                ),
                (
                    "artist.ban",
                    serde_json::json!({
                        "artist_id": id, "name": "worse_artist", "previous_name": "bad_artist"
                    })
                ),
                (
                    "artist.unban",
                    serde_json::json!({ "artist_id": id, "name": "worse_artist", "deleted": true })
                ),
            ]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn urls_are_kept_encoded_and_shown_escaped(pool: PgPool) {
        let state = test_state(&pool).await;
        let evil = "https://twitter.com/a\"><form data-upload>'";
        state.sources.remember(
            evil,
            crate::sources::SourceInfo {
                site: "Example",
                page_url: evil.to_owned(),
                profile_urls: vec![evil.to_owned()],
                artist_name: Some("someone".into()),
                ..crate::sources::SourceInfo::default()
            },
        );
        let app = TestApp::new(
            state,
            super::routes().merge(crate::uploads::routes(1024 * 1024)),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        // Typed encoded, a profile's canonical form stays encoded.
        let created = app
            .post_form(
                "/artists",
                Some(&alice),
                &[],
                "name=cat_artist&urls=https%3A%2F%2Fmisskey.io%2F%40a%2522%253E%253Cb%2527",
            )
            .await;
        assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
        let id = id_from(&created.location.unwrap());
        let stored = moekura_db::artists::urls(&pool, &[id]).await.unwrap();
        assert_eq!(stored[0].url, "https://misskey.io/@a%22%3E%3Cb%27");
        // One that only grows past the limit once encoded is refused as
        // too long, not left to the table to fail.
        let long = app
            .post_form(
                "/artists",
                Some(&alice),
                &[],
                &format!(
                    "name=long_artist&urls=https%3A%2F%2Fexample.com%2F{}",
                    "%27".repeat(1000)
                ),
            )
            .await;
        assert_eq!(
            long.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{}",
            long.body
        );
        assert!(
            long.body.contains("at most 2048 characters"),
            "{}",
            long.body
        );

        // One stored raw before that is escaped wherever it's linked.
        sqlx::query("UPDATE artist_urls SET url = $1 WHERE artist_id = $2")
            .bind(evil)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let escaped = "https://twitter.com/a&quot;&gt;&lt;form data-upload&gt;&#x27;";
        let query: String = url::form_urlencoded::byte_serialize(evil.as_bytes()).collect();
        for path in [
            format!("/artists/{id}"),
            "/artists".to_owned(),
            format!("/uploads/source-data?url={query}"),
        ] {
            let body = app.get(&path, Some(&alice)).await.body;
            assert!(!body.contains("<form data-upload"), "{path}: {body}");
            assert!(
                body.contains(&format!("href=\"{escaped}\"")),
                "{path}: {body}"
            );
        }
    }
}
