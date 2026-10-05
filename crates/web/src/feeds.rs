//! Atom feeds: the newest posts of a search (`/posts.atom?tags=…`) and the
//! newest comments (`/comments.atom`). Feed readers can't log in, so on
//! private sites they pass a user's feed token (`?token=…`), which
//! reads as that user, seeing no more than a member, and does nothing
//! else.

use std::collections::HashMap;
use std::fmt::Write;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, VARY};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use moekura_core::markup;
use moekura_core::permissions::Permission;
use moekura_core::posts::PostStatus;
use moekura_core::search::Query as SearchQuery;
use moekura_core::tokens::NewToken;
use moekura_db::comments::{self, Filter};
use moekura_db::posts::Visibility;
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

/// How long readers and proxies may keep a feed read by a visitor.
const MAX_AGE: &str = "public, max-age=300";

/// How long a reader may keep a feed read as someone; shared caches
/// mustn't.
const PRIVATE_MAX_AGE: &str = "private, max-age=300";

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

/// Who reads a feed, and the posts it may show them.
struct Reader {
    current: CurrentUser,
    seen: Visibility,
}

/// Who a feed is read as: the token's user, or the requester, if they
/// may see posts. Otherwise a plain 401 rather than the login page, which
/// feed readers can't use.
async fn reader(
    state: &AppState,
    current: CurrentUser,
    token: Option<&str>,
) -> Result<Result<Reader, &'static str>, AppError> {
    let (current, seen) = match token.filter(|t| !t.is_empty()) {
        None => {
            let seen = visibility(&current);
            (current, seen)
        }
        Some(token) => match feeds::user(state.db.primary(), token).await? {
            Some((user, ban)) => {
                let current = CurrentUser::for_user(user, ban, &state.site.get());
                let seen = member_visibility(state, &current);
                (current, seen)
            }
            None => return Ok(Err("This feed token is wrong or was revoked.")),
        },
    };
    if !current.can(Permission::ViewPosts) {
        return Ok(Err(
            "This site is private: add a feed token (see your settings) to the feed's address.",
        ));
    }
    Ok(Ok(Reader { current, seen }))
}

/// What a feed read with a token may show: no more than a member sees,
/// whatever the token's user may, since the token sits in a feed reader's
/// list and never expires. Their own pending uploads and safe mode still
/// count.
fn member_visibility(state: &AppState, current: &CurrentUser) -> Visibility {
    let mut seen = visibility(current);
    seen.statuses
        .retain(|s| matches!(s, PostStatus::Active | PostStatus::Flagged));
    seen.deleted_by_default = false;
    let site = state.site.get();
    if site.settings.banned_artists.hide_posts {
        seen.hidden_tags = site.banned_artist_tags().to_vec();
    }
    seen
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

/// Escapes text for XML, dropping characters XML 1.0 doesn't allow, any
/// one of which would make the whole document unreadable.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if !xml_char(c) => {}
            c => out.push(c),
        }
    }
    out
}

/// Whether `c` may be in an XML 1.0 document (its `Char` production),
/// leaving out the control characters it merely discourages too.
fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r')
        || (matches!(c, '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
            && !c.is_control())
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

/// The feed. Only one a visitor read on a public site is for shared
/// caches: read with a session or a token, it shows what that user may
/// see and leaves out what their blacklist does.
fn respond(xml: String, shareable: bool) -> Response {
    let cache = if shareable { MAX_AGE } else { PRIVATE_MAX_AGE };
    (
        [
            (CONTENT_TYPE, "application/atom+xml; charset=utf-8"),
            (CACHE_CONTROL, cache),
            // The same address reads differently with a session cookie.
            (VARY, "Cookie"),
        ],
        xml,
    )
        .into_response()
}

/// Whether a feed read as `current` is the same for every visitor.
fn shareable(state: &AppState, current: &CurrentUser) -> bool {
    !state.is_private() && !current.is_logged_in()
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
    let Reader { current, seen } = match reader(&state, current, query.token.as_deref()).await? {
        Ok(reader) => reader,
        Err(message) => return Ok(refused(message)),
    };
    let db = state.reader(&current);
    let parsed =
        SearchQuery::parse(&query.tags).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let normalized = parsed.to_string();
    let config = moekura_core::config::SearchConfig {
        per_page: ENTRIES,
        ..state.search_config()
    };
    let mut plan = Plan::resolve(db, &parsed, &seen, &config)
        .await
        .map_err(search_error)?;
    // The reader's own blacklist, when read as someone who has one.
    if current.is_logged_in()
        && let Some(list) = crate::blacklist::for_viewer(&state, db, &current).await?
    {
        plan.exclude(&list.exclusions());
    }
    let ids = plan
        .ids(db, PageRef::Number(1))
        .await
        .map_err(search_error)?;
    let found = posts::by_ids(db, &ids).await?;
    let sizes = &state.media.config().thumbnail_sizes;
    let box_size = sizes.first().copied().unwrap_or(250);
    let kind = format!("thumb-{box_size}");
    let cards = posts::cards(db, &ids, (&kind, &kind)).await?;
    // Every entry's tags and uploader, looked up at once.
    let mut tag_ids: Vec<i32> = found.iter().flat_map(|p| p.tag_ids.clone()).collect();
    tag_ids.sort_unstable();
    tag_ids.dedup();
    let tag_names: HashMap<i32, String> = tags::by_ids(db, &tag_ids)
        .await?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect();
    let mut uploader_ids: Vec<i64> = found.iter().filter_map(|p| p.uploader_id).collect();
    uploader_ids.sort_unstable();
    uploader_ids.dedup();
    let uploaders: HashMap<i64, String> = moekura_db::users::names(db, &uploader_ids)
        .await?
        .into_iter()
        .collect();
    let mut entries = Vec::new();
    for id in &ids {
        let Some(post) = found.iter().find(|p| p.id == *id) else {
            continue;
        };
        let mut names: Vec<&str> = post
            .tag_ids
            .iter()
            .filter_map(|id| tag_names.get(id).map(String::as_str))
            .collect();
        names.sort_unstable();
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
        let uploader = post
            .uploader_id
            .and_then(|user| uploaders.get(&user).cloned());
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
    Ok(respond(xml, shareable(&state, &current)))
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
    let Reader { current, seen } = match reader(&state, current, query.token.as_deref()).await? {
        Ok(reader) => reader,
        Err(message) => return Ok(refused(message)),
    };
    let db = state.reader(&current);
    let found = comments::list(
        db,
        &seen,
        &Filter::default(),
        None,
        0,
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
    Ok(respond(xml, shareable(&state, &current)))
}

#[derive(Debug, Deserialize)]
struct TokenForm {
    /// `new` or `revoke`.
    action: String,
    /// Needed for a new token, which is a way in.
    #[serde(default)]
    password: String,
}

/// Makes a new feed token (replacing any other), shown once, or revokes
/// it. Making one takes the password and a session, not an API key.
async fn feed_token(
    page: Page,
    jar: CookieJar,
    Form(form): Form<TokenForm>,
) -> Result<Response, AppError> {
    let user = page.current.require_session()?;
    let db = page.state().db.primary();
    match form.action.as_str() {
        "new" => {
            if let Err(message) =
                crate::auth::confirm_password(page.state(), &page.current, &form.password).await?
            {
                return Err(AppError::Unprocessable(message));
            }
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
        moekura_db::comments::create(&pool, cat, alice, "So [b]fluffy[/b]", true)
            .await
            .unwrap();
        // XML has no place for these, and one would spoil the whole feed.
        moekura_db::comments::create(&pool, cat, alice, "Odd\u{FFFF}\u{FFFE} one", true)
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
        assert!(comments.body.contains("Odd one"), "{}", comments.body);
    }

    #[test]
    fn escapes_for_xml() {
        assert_eq!(
            escape("<a href=\"x\">Tom & Jerry's</a>"),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; Jerry&apos;s&lt;/a&gt;"
        );
        // Only what XML 1.0 allows is kept.
        assert_eq!(
            escape("a\u{0}b\u{1F}c\u{7F}\u{85}d\u{FFFE}\u{FFFF}e\u{D7FF}\u{E000}\u{FFFD}\u{10000}"),
            "abcde\u{D7FF}\u{E000}\u{FFFD}\u{10000}"
        );
        assert_eq!(escape("line\r\n\tend"), "line\r\n\tend");
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

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn new_tokens_take_the_password_and_a_session(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            routes().merge(crate::users::routes()),
        );
        let (bob, session) = crate::test_support::member(&pool, "bob", "bob@example.com").await;
        assert!(
            app.get("/settings", Some(&session))
                .await
                .body
                .contains("id=\"feed-password\"")
        );
        let has_token = || async { feeds::has_token(&pool, bob.id).await.unwrap() };
        let wrong = app
            .post_form(
                "/settings/feed-token",
                Some(&session),
                &[],
                "action=new&password=nope",
            )
            .await;
        assert_eq!(wrong.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!has_token().await);

        let key = moekura_db::api_keys::create(&pool, bob.id, "bot", None)
            .await
            .unwrap();
        let by_key = app
            .post_form(
                "/settings/feed-token",
                None,
                &[("authorization", &format!("Bearer {key}"))],
                "action=new&password=correct+horse",
            )
            .await;
        assert_eq!(by_key.status, StatusCode::FORBIDDEN, "{}", by_key.body);
        assert!(!has_token().await);

        let made = app
            .post_form(
                "/settings/feed-token",
                Some(&session),
                &[],
                "action=new&password=correct+horse",
            )
            .await;
        assert_eq!(made.status, StatusCode::OK, "{}", made.body);
        assert!(has_token().await);
        // Revoking needs no password.
        app.post_form("/settings/feed-token", Some(&session), &[], "action=revoke")
            .await;
        assert!(!has_token().await);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn without_a_password_a_token_takes_a_fresh_login(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            routes().merge(crate::users::routes()),
        );
        // Made through single sign-on, logged in a while ago.
        let session = session_for(&pool, "bob", SystemRole::Member).await;
        let bob = moekura_db::users::by_name(&pool, "bob")
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE sessions SET created_at = now() - interval '11 minutes'")
            .execute(&pool)
            .await
            .unwrap();
        let settings = app.get("/settings", Some(&session)).await;
        assert!(!settings.body.contains("id=\"feed-password\""));
        assert!(
            settings.body.contains("within 10 minutes"),
            "{}",
            settings.body
        );
        let stale = app
            .post_form("/settings/feed-token", Some(&session), &[], "action=new")
            .await;
        assert_eq!(stale.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(stale.body.contains("log in again"), "{}", stale.body);
        assert!(!feeds::has_token(&pool, bob.id).await.unwrap());
    }

    /// The posts in a posts feed, as numbered in their titles.
    fn entries(feed: &str) -> Vec<i64> {
        let mut ids: Vec<i64> = feed
            .split("<title>Post #")
            .skip(1)
            .filter_map(|rest| rest.split('<').next()?.parse().ok())
            .collect();
        ids.sort_unstable();
        ids
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn tokens_read_no_more_than_a_member(pool: PgPool) {
        // A banned artist's post, hidden from all but staff.
        let banned = post(&pool, &["banned_artist"], "g").await;
        sqlx::query("INSERT INTO artists (name, is_banned) VALUES ('banned_artist', true)")
            .execute(&pool)
            .await
            .unwrap();
        let app = TestApp::new(test_state(&pool).await, routes());
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let root: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'root'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let active = post(&pool, &["cat"], "g").await;
        let mut moderated = Vec::new();
        for (status, uploader) in [
            ("pending", None),
            ("deleted", None),
            ("pending", Some(root)),
        ] {
            let id = post(&pool, &["cat"], "g").await;
            sqlx::query("UPDATE posts SET status = $2, uploader_id = $3 WHERE id = $1")
                .bind(id)
                .bind(status)
                .bind(uploader)
                .execute(&pool)
                .await
                .unwrap();
            moderated.push(id);
        }
        let [pending, deleted, own] = moderated[..] else {
            unreachable!()
        };
        let token = NewToken::generate();
        feeds::set_token(&pool, root, Some(&token.hash))
            .await
            .unwrap();

        // Logged in, an admin sees all of it...
        let staff = app.get("/posts.atom?tags=status:any", Some(&admin)).await;
        assert_eq!(
            entries(&staff.body),
            [banned, active, pending, deleted, own],
            "{}",
            staff.body
        );
        // ...but their token, whoever finds it, only what a member would,
        // and their own upload.
        let feed = app
            .get(
                &format!("/posts.atom?tags=status:any&token={}", token.token),
                None,
            )
            .await;
        assert_eq!(entries(&feed.body), [active, own], "{}", feed.body);
        for search in ["status:pending", "status:deleted", "banned_artist"] {
            let feed = app
                .get(
                    &format!("/posts.atom?tags={search}&token={}", token.token),
                    None,
                )
                .await;
            assert!(
                entries(&feed.body).iter().all(|id| *id == own),
                "{search}: {}",
                feed.body
            );
        }

        for id in [active, pending, deleted] {
            moekura_db::comments::create(&pool, id, root, &format!("On {id}"), true)
                .await
                .unwrap();
        }
        let comments = app
            .get(&format!("/comments.atom?token={}", token.token), None)
            .await;
        assert_eq!(
            comments.body.matches("<entry>").count(),
            1,
            "{}",
            comments.body
        );
        assert!(comments.body.contains(&format!("On {active}")));
        let staff = app.get("/comments.atom", Some(&admin)).await;
        assert_eq!(staff.body.matches("<entry>").count(), 3, "{}", staff.body);
    }

    /// `Cache-Control` and every `Vary` of a feed read with `cookie`.
    async fn caching(app: &TestApp, path: &str, cookie: Option<&str>) -> (String, Vec<String>) {
        let mut request = axum::http::Request::get(path);
        if let Some(cookie) = cookie {
            request = request.header(axum::http::header::COOKIE, cookie);
        }
        let response = app
            .raw(request.body(axum::body::Body::empty()).unwrap())
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let headers = response.headers();
        (
            headers[CACHE_CONTROL].to_str().unwrap().to_owned(),
            headers
                .get_all(VARY)
                .iter()
                .map(|v| v.to_str().unwrap().to_owned())
                .collect(),
        )
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn only_visitors_feeds_are_for_shared_caches(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, routes());
        post(&pool, &["cat"], "g").await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let cookie = format!("{}={admin}", crate::auth::SESSION_COOKIE);
        let token = NewToken::generate();
        let root: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = 'root'")
            .fetch_one(&pool)
            .await
            .unwrap();
        feeds::set_token(&pool, root, Some(&token.hash))
            .await
            .unwrap();
        let public = "public, max-age=300".to_owned();
        let private = "private, max-age=300".to_owned();
        for path in ["/posts.atom?tags=cat", "/comments.atom"] {
            let (cache, vary) = caching(&app, path, None).await;
            assert_eq!(cache, public, "{path}");
            assert!(vary.iter().any(|v| v == "Cookie"), "{path}: {vary:?}");
            // Read as staff: theirs alone, even on a public site.
            let (cache, vary) = caching(&app, path, Some(&cookie)).await;
            assert_eq!(cache, private, "{path}");
            assert!(vary.iter().any(|v| v == "Cookie"), "{path}: {vary:?}");
            let with_token = format!(
                "{path}{}token={}",
                if path.contains('?') { '&' } else { '?' },
                token.token
            );
            assert_eq!(caching(&app, &with_token, None).await.0, private);
        }
    }
}
