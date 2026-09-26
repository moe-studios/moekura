//! Atom feeds: the newest posts of a search (`/posts.atom?tags=…`) and the
//! newest comments (`/comments.atom`). Feed readers can't log in, so on
//! private sites they pass a user's feed token (`?token=…`), which
//! reads as that user and does nothing else.

use std::fmt::Write;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_core::search::Query as SearchQuery;
use moekura_core::tokens::NewToken;
use moekura_db::comments::{self, Filter};
use moekura_db::search::{PageRef, Plan, SearchError};
use moekura_db::{feeds, posts, tags};
use serde::Deserialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::AppState;
use crate::api::absolute_url;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::posts::visibility;

/// Entries in a feed, unless the search asks for fewer (`limit:`).
const ENTRIES: u32 = 40;

/// How long readers and proxies may keep a feed.
const MAX_AGE: &str = "public, max-age=300";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/posts.atom", get(posts_feed))
        .route("/comments.atom", get(comments_feed))
        .route("/settings/feed-token", post(feed_token))
}

#[derive(Debug, Default, Deserialize)]
struct FeedQuery {
    #[serde(default)]
    tags: String,
    /// A feed token, for private sites.
    token: Option<String>,
}

/// Who a feed is read as: the token's user, or the requester, if they
/// may see posts. Otherwise a plain 401 rather than the login page, which
/// feed readers can't use.
async fn reader(
    state: &AppState,
    current: CurrentUser,
    token: Option<&str>,
) -> Result<Result<CurrentUser, &'static str>, AppError> {
    let current = match token.filter(|t| !t.is_empty()) {
        None => current,
        Some(token) => match feeds::user(state.db.primary(), token).await? {
            Some((user, ban)) => CurrentUser::for_user(user, ban, &state.site.get()),
            None => return Ok(Err("This feed token is wrong or was revoked.")),
        },
    };
    if !current.can(Permission::ViewPosts) {
        return Ok(Err(
            "This site is private: add a feed token (see your settings) to the feed's address.",
        ));
    }
    Ok(Ok(current))
}

/// A refused feed: a plain 401 with why.
fn refused(message: &'static str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        message,
    )
        .into_response()
}

/// Escapes text for XML.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Not allowed in XML 1.0.
            c if c.is_control() && !matches!(c, '\n' | '\r' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_default()
}

/// An entry: its id and link, title, time, author and HTML content.
struct Entry {
    url: String,
    title: String,
    updated: OffsetDateTime,
    author: Option<String>,
    html: String,
}

/// The feed document.
fn atom(
    state: &AppState,
    self_url: &str,
    page_url: &str,
    title: &str,
    entries: &[Entry],
) -> String {
    let site = state.site.get();
    let updated = entries
        .iter()
        .map(|e| e.updated)
        .max()
        .unwrap_or_else(OffsetDateTime::now_utc);
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    let _ = write!(
        xml,
        "<feed xmlns=\"http://www.w3.org/2005/Atom\">\n\
         <id>{id}</id>\n<title>{title}</title>\n<updated>{updated}</updated>\n\
         <link rel=\"self\" type=\"application/atom+xml\" href=\"{id}\"/>\n\
         <link rel=\"alternate\" type=\"text/html\" href=\"{page}\"/>\n\
         <generator uri=\"https://github.com/moe-studios/moekura\">Moekura</generator>\n\
         <author><name>{site}</name></author>\n",
        id = escape(self_url),
        title = escape(title),
        updated = rfc3339(updated),
        page = escape(page_url),
        site = escape(&site.settings.site_name),
    );
    for entry in entries {
        let _ = write!(
            xml,
            "<entry>\n<id>{url}</id>\n<title>{title}</title>\n<updated>{updated}</updated>\n\
             <link rel=\"alternate\" type=\"text/html\" href=\"{url}\"/>\n",
            url = escape(&entry.url),
            title = escape(&entry.title),
            updated = rfc3339(entry.updated),
        );
        if let Some(author) = &entry.author {
            let _ = writeln!(xml, "<author><name>{}</name></author>", escape(author));
        }
        let _ = write!(
            xml,
            "<content type=\"html\">{}</content>\n</entry>\n",
            escape(&entry.html)
        );
    }
    xml.push_str("</feed>\n");
    xml
}

fn respond(xml: String, private: bool) -> Response {
    // Feeds of private sites (read with a token) aren't for shared caches.
    let cache = if private {
        "private, max-age=300"
    } else {
        MAX_AGE
    };
    (
        [
            (CONTENT_TYPE, "application/atom+xml; charset=utf-8"),
            (CACHE_CONTROL, cache),
        ],
        xml,
    )
        .into_response()
}

/// The feed's own URL, without the token (the id of a feed shouldn't
/// carry a secret).
fn self_url(state: &AppState, path: &str, tags: &str) -> String {
    let url = if tags.is_empty() {
        path.to_owned()
    } else {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("tags", tags)
            .finish();
        format!("{path}?{q}")
    };
    absolute_url(state, &url)
}

async fn posts_feed(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<FeedQuery>,
) -> Result<Response, AppError> {
    let current = match reader(&state, current, query.token.as_deref()).await? {
        Ok(current) => current,
        Err(message) => return Ok(refused(message)),
    };
    let db = state.reader(&current);
    let parsed =
        SearchQuery::parse(&query.tags).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let normalized = parsed.to_string();
    let config = moekura_core::config::SearchConfig {
        per_page: ENTRIES,
        ..state.config.search.clone()
    };
    let plan = Plan::resolve(db, &parsed, &visibility(&current), &config)
        .await
        .map_err(search_error)?;
    let ids = plan
        .ids(db, PageRef::Number(1))
        .await
        .map_err(search_error)?;
    let found = posts::by_ids(db, &ids).await?;
    // The reader's own blacklist, when read as someone who has one.
    let blacklist = if current.is_logged_in() {
        crate::blacklist::for_viewer(&state, db, &current).await?
    } else {
        None
    };
    let sizes = &state.media.config().thumbnail_sizes;
    let box_size = sizes.first().copied().unwrap_or(250);
    let kind = format!("thumb-{box_size}");
    let cards = posts::cards(db, &ids, (&kind, &kind)).await?;
    let mut entries = Vec::new();
    for id in &ids {
        let Some(post) = found.iter().find(|p| p.id == *id) else {
            continue;
        };
        if blacklist
            .as_ref()
            .is_some_and(|list| list.matching(post.rating, &post.tag_ids).is_some())
        {
            continue;
        }
        let mut names: Vec<String> = tags::by_ids(db, &post.tag_ids)
            .await?
            .into_iter()
            .map(|t| t.name)
            .collect();
        names.sort();
        let url = absolute_url(&state, &format!("/posts/{id}"));
        let thumb = cards
            .iter()
            .find(|c| c.id == *id)
            .and_then(|c| c.thumb.as_deref())
            .and_then(moekura_storage::Key::parse)
            .map(|key| absolute_url(&state, &state.file_url(&key)));
        let mut html = String::new();
        if let Some(thumb) = thumb {
            let _ = write!(
                html,
                "<p><a href=\"{url}\"><img src=\"{}\" alt=\"Post #{id}\"></a></p>",
                escape(&thumb)
            );
        }
        let _ = write!(
            html,
            "<p>{}</p><p>Rating: {}</p>",
            escape(&names.join(" ")),
            post.rating.label()
        );
        let uploader = match post.uploader_id {
            Some(user) => moekura_db::users::by_id(db, user).await?.map(|u| u.name),
            None => None,
        };
        entries.push(Entry {
            url,
            title: format!("Post #{id}"),
            updated: post.created_at,
            author: uploader,
            html,
        });
    }
    let site = state.site.get().settings.site_name.clone();
    let title = if normalized.is_empty() {
        format!("{site}: newest posts")
    } else {
        format!("{site}: {normalized}")
    };
    let page_url = absolute_url(&state, &crate::templates::search_url(&normalized));
    let xml = atom(
        &state,
        &self_url(&state, "/posts.atom", &normalized),
        &page_url,
        &title,
        &entries,
    );
    Ok(respond(xml, state.is_private()))
}

fn search_error(error: SearchError) -> AppError {
    match error {
        SearchError::Invalid(message) => AppError::BadRequest(message),
        SearchError::Db(e) => e.into(),
    }
}

async fn comments_feed(
    State(state): State<AppState>,
    current: CurrentUser,
    Query(query): Query<FeedQuery>,
) -> Result<Response, AppError> {
    let current = match reader(&state, current, query.token.as_deref()).await? {
        Ok(current) => current,
        Err(message) => return Ok(refused(message)),
    };
    let db = state.reader(&current);
    let found = comments::list(
        db,
        &visibility(&current),
        &Filter::default(),
        None,
        i64::from(ENTRIES),
    )
    .await?;
    let entries: Vec<Entry> = found
        .into_iter()
        .map(|c| Entry {
            url: absolute_url(&state, &crate::comments::url(&c)),
            title: format!(
                "{} on post #{}",
                c.creator_name.as_deref().unwrap_or("Someone"),
                c.post_id
            ),
            updated: c.edited_at.unwrap_or(c.created_at),
            author: c.creator_name.clone(),
            html: markup::render(&c.body),
        })
        .collect();
    let site = state.site.get().settings.site_name.clone();
    let xml = atom(
        &state,
        &self_url(&state, "/comments.atom", ""),
        &absolute_url(&state, "/comments"),
        &format!("{site}: comments"),
        &entries,
    );
    Ok(respond(xml, state.is_private()))
}

#[derive(Debug, Deserialize)]
struct TokenForm {
    /// `new` or `revoke`.
    action: String,
}

/// Makes a new feed token (replacing any other), shown once, or revokes
/// it.
async fn feed_token(
    page: Page,
    jar: CookieJar,
    Form(form): Form<TokenForm>,
) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let db = page.state().db.primary();
    match form.action.as_str() {
        "new" => {
            let token = NewToken::generate();
            feeds::set_token(db, user.id, Some(&token.hash)).await?;
            let q = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("tags", "")
                .append_pair("token", &token.token)
                .finish();
            let example = absolute_url(page.state(), &format!("/posts.atom?{q}"));
            Ok(page.render_with_status(
                StatusCode::OK,
                "feed_token.html",
                minijinja::context! { token => token.token, example => example },
            ))
        }
        "revoke" => {
            feeds::set_token(db, user.id, None).await?;
            Ok((flash::set(jar, Flash::Saved), Redirect::to("/settings")).into_response())
        }
        _ => Err(AppError::BadRequest("Unknown action".into())),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, session_for, test_state};

    async fn post(pool: &PgPool, tags: &[&str], rating: &str) -> i64 {
        let mut ids = Vec::new();
        for name in tags {
            ids.push(
                sqlx::query_scalar::<_, i32>(
                    "INSERT INTO tags (name) VALUES ($1)
                     ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name RETURNING id",
                )
                .bind(name)
                .fetch_one(pool)
                .await
                .unwrap(),
            );
        }
        ids.sort_unstable();
        sqlx::query_scalar("INSERT INTO posts (rating, tag_ids) VALUES ($1, $2) RETURNING id")
            .bind(rating)
            .bind(ids)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn post_and_comment_feeds(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            routes().merge(crate::users::routes()),
        );
        let cat = post(&pool, &["cat", "a<b"], "g").await;
        let dog = post(&pool, &["dog"], "e").await;
        let feed = app.get("/posts.atom?tags=cat", None).await;
        assert_eq!(feed.status, StatusCode::OK);
        assert!(feed.body.starts_with("<?xml"), "{}", feed.body);
        assert!(feed.body.contains(&format!("<title>Post #{cat}</title>")));
        assert!(!feed.body.contains(&format!("Post #{dog}")));
        // Twice escaped: once as HTML, once as XML.
        assert!(feed.body.contains("a&amp;lt;b cat"), "{}", feed.body);
        assert!(feed.body.contains("<title>Moekura: cat</title>"));
        let all = app.get("/posts.atom", None).await;
        assert!(
            all.body.contains(&format!("Post #{dog}"))
                && all.body.contains(&format!("Post #{cat}"))
        );
        assert_eq!(
            app.get("/posts.atom?tags=~", None).await.status,
            StatusCode::BAD_REQUEST
        );

        let alice: i64 = {
            session_for(&pool, "alice", SystemRole::Member).await;
            sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        moekura_db::comments::create(&pool, cat, alice, "So [b]fluffy[/b]")
            .await
            .unwrap();
        let comments = app.get("/comments.atom", None).await;
        assert!(
            comments.body.contains("<title>alice on post #"),
            "{}",
            comments.body
        );
        assert!(
            comments
                .body
                .contains("So &lt;strong&gt;fluffy&lt;/strong&gt;")
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn tokens_open_private_sites(pool: PgPool) {
        sqlx::query("UPDATE roles SET permissions = 0 WHERE system_key = 'anonymous'")
            .execute(&pool)
            .await
            .unwrap();
        let app = TestApp::new(
            test_state(&pool).await,
            routes().merge(crate::users::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        post(&pool, &["cat"], "e").await;
        post(&pool, &["cat"], "g").await;
        assert_eq!(
            app.get("/posts.atom", None).await.status,
            StatusCode::UNAUTHORIZED
        );

        let made = app
            .post_form("/settings/feed-token", Some(&alice), &[], "action=new")
            .await;
        assert_eq!(made.status, StatusCode::OK, "{}", made.body);
        let token = made
            .body
            .split("<code class=\"token\">")
            .nth(1)
            .and_then(|rest| rest.split('<').next())
            .unwrap()
            .to_owned();
        assert!(
            app.get("/settings", Some(&alice))
                .await
                .body
                .contains("Revoke it")
        );

        let feed = app.get(&format!("/posts.atom?token={token}"), None).await;
        assert_eq!(feed.status, StatusCode::OK, "{}", feed.body);
        assert_eq!(feed.body.matches("<entry>").count(), 2);
        assert!(
            feed.body
                .contains("<id>http://localhost:8080/posts.atom</id>"),
            "the id has no token"
        );
        // Read as alice, with her blacklist.
        moekura_db::users::set_settings(
            &pool,
            sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
                .fetch_one(&pool)
                .await
                .unwrap(),
            &serde_json::json!({ "blacklist": "rating:e" }),
        )
        .await
        .unwrap();
        let feed = app.get(&format!("/posts.atom?token={token}"), None).await;
        assert_eq!(feed.body.matches("<entry>").count(), 1);

        assert_eq!(
            app.get("/posts.atom?token=wrong", None).await.status,
            StatusCode::UNAUTHORIZED
        );
        app.post_form("/settings/feed-token", Some(&alice), &[], "action=revoke")
            .await;
        assert_eq!(
            app.get(&format!("/posts.atom?token={token}"), None)
                .await
                .status,
            StatusCode::UNAUTHORIZED
        );
    }
}
