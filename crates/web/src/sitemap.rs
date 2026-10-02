//! `/robots.txt` and the sitemap (`/sitemap.xml`, an index of files under
//! `/sitemaps/`) of posts, tags, wiki pages, pools, artists and forum
//! topics. Private sites ask crawlers to stay out and have no sitemap.

use std::fmt::Write as _;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use moekura_db::sitemap::{self, Kind, PostFilter};
use time::format_description::well_known::Rfc3339;

use crate::AppState;
use crate::error::AppError;
use crate::feeds::escape;

/// How long crawlers and caches may keep these.
const CACHE: &str = "public, max-age=3600";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/robots.txt", get(robots))
        .route("/sitemap.xml", get(index))
        .route("/sitemaps/{file}", get(file))
}

/// Pages crawlers are asked to leave alone on public sites: accounts,
/// staff pages, history, the APIs and searches beyond a single tag.
const DISALLOWED: &[&str] = &[
    "/admin",
    "/moderation",
    "/settings",
    "/login",
    "/register",
    "/forgot-password",
    "/reset-password",
    "/verify-email",
    "/dmails",
    "/notifications",
    "/invites",
    "/upload",
    "/uploads",
    "/saved_searches",
    "/api/",
    "/redirect",
    "/news_updates",
    "/*.json",
    "/*.atom",
    "/*/history",
    "/*/edit",
    "/post_versions",
    "/tag_versions",
    "/note_versions",
    "/pool_versions",
    "/wiki_page_versions",
    "/artist_versions",
    "/artist_commentary_versions",
    "/explore/posts/searches",
    "/explore/posts/missed_searches",
    "/posts/*/next",
    "/posts/*/prev",
    "/posts?*page=",
    "/posts?*+",
    "/posts?*%20",
    "/posts?*%3A",
    "/forum_posts?",
    "/comments?",
];

/// The `robots.txt` served when the site sets none.
pub(crate) fn default_robots(state: &AppState) -> String {
    if state.is_private() {
        return "User-agent: *\nDisallow: /\n".to_owned();
    }
    let mut text = "User-agent: *\n".to_owned();
    for path in DISALLOWED {
        let _ = writeln!(text, "Disallow: {path}");
    }
    let _ = write!(text, "\nSitemap: {}\n", link(state, "/sitemap.xml"));
    text
}

fn link(state: &AppState, path: &str) -> String {
    state
        .config
        .server
        .public_url
        .join(path)
        .map_or_else(|_| path.to_owned(), String::from)
}

async fn robots(State(state): State<AppState>) -> Response {
    let custom = state.site.get().settings.robots_txt.clone();
    let text = if custom.trim().is_empty() {
        default_robots(&state)
    } else {
        custom
    };
    (
        [
            (CONTENT_TYPE, "text/plain; charset=utf-8"),
            (CACHE_CONTROL, CACHE),
        ],
        text,
    )
        .into_response()
}

fn xml(body: String) -> Response {
    (
        [
            (CONTENT_TYPE, "application/xml; charset=utf-8"),
            (CACHE_CONTROL, CACHE),
        ],
        body,
    )
        .into_response()
}

const HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>"#;

async fn index(State(state): State<AppState>) -> Result<Response, AppError> {
    if state.is_private() {
        return Err(AppError::NotFound);
    }
    let db = state.db.read();
    let mut body =
        format!("{HEAD}\n<sitemapindex xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for kind in Kind::ALL {
        for n in 0..sitemap::files(db, kind).await? {
            let url = link(&state, &format!("/sitemaps/{}-{n}.xml", kind.as_str()));
            let _ = writeln!(body, "<sitemap><loc>{}</loc></sitemap>", escape(&url));
        }
    }
    body.push_str("</sitemapindex>\n");
    Ok(xml(body))
}

/// `posts-3.xml` as its kind and number.
fn parse_file(name: &str) -> Option<(Kind, i64)> {
    let (kind, n) = name.strip_suffix(".xml")?.rsplit_once('-')?;
    Some((Kind::parse(kind)?, n.parse().ok().filter(|n| *n >= 0)?))
}

fn encode(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

/// The page an entry of `kind` stands for.
fn page_path(kind: Kind, key: &str) -> String {
    match kind {
        Kind::Posts => format!("/posts/{key}"),
        Kind::Tags => format!("/posts?tags={}", encode(key)),
        // Paths take %20, not +.
        Kind::Wiki => format!("/wiki/{}", encode(key).replace('+', "%20")),
        Kind::Pools => format!("/pools/{key}"),
        Kind::Artists => format!("/artists/{key}"),
        Kind::Topics => format!("/forum_topics/{key}"),
    }
}

async fn file(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    if state.is_private() {
        return Err(AppError::NotFound);
    }
    let (kind, n) = parse_file(&name).ok_or(AppError::NotFound)?;
    let db = state.db.read();
    if n >= sitemap::files(db, kind).await? {
        return Err(AppError::NotFound);
    }
    let site = state.site.get();
    let settings = &site.settings;
    let filter = PostFilter {
        ratings: settings
            .visitor_ratings
            .iter()
            .map(|r| r.code().to_owned())
            .collect(),
        hidden_tags: if settings.banned_artists.hide_posts {
            site.banned_artist_tags().to_vec()
        } else {
            Vec::new()
        },
    };
    let entries = sitemap::entries(db, kind, n, &filter).await?;
    let mut body =
        format!("{HEAD}\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for entry in entries {
        let url = link(&state, &page_path(kind, &entry.key));
        let _ = write!(body, "<url><loc>{}</loc>", escape(&url));
        if let Some(at) = entry.updated_at.and_then(|at| at.format(&Rfc3339).ok()) {
            let _ = write!(body, "<lastmod>{at}</lastmod>");
        }
        body.push_str("</url>\n");
    }
    body.push_str("</urlset>\n");
    Ok(xml(body))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::Permissions;
    use sqlx::PgPool;

    use super::*;
    use crate::test_support::{TestApp, test_state};

    #[test]
    fn file_names() {
        assert_eq!(parse_file("posts-3.xml"), Some((Kind::Posts, 3)));
        assert_eq!(parse_file("forum-0.xml"), Some((Kind::Topics, 0)));
        assert_eq!(parse_file("posts--1.xml"), None);
        assert_eq!(parse_file("nope-1.xml"), None);
        assert_eq!(
            page_path(Kind::Wiki, "long hair/x"),
            "/wiki/long%20hair%2Fx"
        );
        assert_eq!(page_path(Kind::Tags, "a&b"), "/posts?tags=a%26b");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn robots_and_sitemaps(pool: PgPool) {
        sqlx::query("INSERT INTO tags (name) VALUES ('cat')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO posts (rating, tag_ids) SELECT 'g', ARRAY[id] FROM tags WHERE name = 'cat'",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO wiki_pages (title, body) VALUES ('cat ears', 'Ears.')")
            .execute(&pool)
            .await
            .unwrap();
        let app = TestApp::new(test_state(&pool).await, routes());

        let robots = app.get("/robots.txt", None).await.body;
        assert!(robots.contains("Disallow: /admin\n"), "{robots}");
        assert!(
            robots.contains("Sitemap: http://localhost:8080/sitemap.xml"),
            "{robots}"
        );

        let index = app.get("/sitemap.xml", None).await.body;
        assert!(
            index.contains("<loc>http://localhost:8080/sitemaps/posts-0.xml</loc>"),
            "{index}"
        );
        assert!(index.contains("/sitemaps/wiki-0.xml") && !index.contains("/sitemaps/pools-0.xml"));
        let posts = app.get("/sitemaps/posts-0.xml", None).await.body;
        assert!(
            posts.contains("<loc>http://localhost:8080/posts/1</loc><lastmod>"),
            "{posts}"
        );
        let tags = app.get("/sitemaps/tags-0.xml", None).await.body;
        assert!(
            tags.contains("<loc>http://localhost:8080/posts?tags=cat</loc>"),
            "{tags}"
        );
        let wiki = app.get("/sitemaps/wiki-0.xml", None).await.body;
        assert!(wiki.contains("/wiki/cat%20ears</loc>"), "{wiki}");
        assert_eq!(
            app.get("/sitemaps/posts-1.xml", None).await.status,
            StatusCode::NOT_FOUND
        );

        // Admins can replace the robots.txt.
        moekura_db::settings::set(
            &pool,
            "robots_txt",
            serde_json::json!("User-agent: *\nAllow: /\n"),
        )
        .await
        .unwrap();
        let state = test_state(&pool).await;
        let app = TestApp::new(state, routes());
        assert_eq!(
            app.get("/robots.txt", None).await.body,
            "User-agent: *\nAllow: /\n"
        );

        // Private sites keep crawlers out.
        moekura_db::settings::set(&pool, "robots_txt", serde_json::json!(""))
            .await
            .unwrap();
        sqlx::query("UPDATE roles SET permissions = $1 WHERE system_key = 'anonymous'")
            .bind(Permissions::NONE.to_db())
            .execute(&pool)
            .await
            .unwrap();
        let app = TestApp::new(test_state(&pool).await, routes());
        assert_eq!(
            app.get("/robots.txt", None).await.body,
            "User-agent: *\nDisallow: /\n"
        );
        assert_eq!(
            app.get("/sitemap.xml", None).await.status,
            StatusCode::NOT_FOUND
        );
    }
}
