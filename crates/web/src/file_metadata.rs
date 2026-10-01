//! A post file's metadata (EXIF, XMP, PNG text, streams), on its own
//! page behind the post's "Metadata" link.

use axum::Router;
use axum::extract::Path;
use axum::response::Response;
use axum::routing::get;
use minijinja::context;
use moekura_core::permissions::Permission;
use moekura_db::{media, posts};

use crate::AppState;
use crate::error::AppError;
use crate::pages::Page;

pub fn routes() -> Router<AppState> {
    Router::new().route("/posts/{id}/metadata", get(show))
}

/// How a group is headed on the page.
fn group_label(group: &str) -> &str {
    match group {
        "File" => "File",
        "EXIF" => "EXIF",
        "XMP" => "XMP",
        "PNG" => "PNG text",
        "Format" => "Container",
        "Video" => "Video stream",
        "Audio" => "Audio stream",
        other => other,
    }
}

async fn show(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    posts::by_id(db, id)
        .await?
        .filter(|p| crate::posts::visibility(&page.current).allows(p))
        .ok_or(AppError::NotFound)?;
    let metadata = media::metadata_for_post(db, id)
        .await?
        .ok_or(AppError::NotFound)?;
    // In a fixed order of groups, then as stored (by tag).
    let order = ["File", "EXIF", "XMP", "PNG", "Format", "Video", "Audio"];
    let mut groups: Vec<(&str, Vec<(&str, &str)>)> = Vec::new();
    for (key, value) in &metadata {
        let (group, tag) = key.split_once(':').unwrap_or(("Other", key));
        match groups.iter_mut().find(|(g, _)| *g == group) {
            Some((_, fields)) => fields.push((tag, value)),
            None => groups.push((group, vec![(tag, value)])),
        }
    }
    groups.sort_by_key(|(g, _)| order.iter().position(|o| o == g).unwrap_or(order.len()));
    Ok(page.render(
        "post_metadata.html",
        context! {
            post_id => id,
            groups => groups.iter().map(|(group, fields)| context! {
                name => group,
                label => group_label(group),
                fields => fields.iter().map(|(tag, value)| context! {
                    tag => tag,
                    value => value,
                    key => format!("{group}:{tag}"),
                }).collect::<Vec<_>>(),
            }).collect::<Vec<_>>(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn shows_metadata(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let post = crate::danbooru::test_support::upload(&app, &alice, 20, "cat").await;
        sqlx::query(
            "UPDATE media_assets SET metadata = '{\"EXIF:Make\": \"Canon\", \"File:ColorComponents\": \"3\"}'
             WHERE post_id = $1",
        )
        .bind(post)
        .execute(&pool)
        .await
        .unwrap();
        let page = app.get(&format!("/posts/{post}/metadata"), None).await;
        assert_eq!(page.status, StatusCode::OK);
        let file = page.body.find("ColorComponents").unwrap();
        let exif = page.body.find("Canon").unwrap();
        assert!(file < exif, "File comes first: {}", page.body);
        assert!(
            page.body.contains("exif%3Aexif%3Amake%3Dcanon"),
            "{}",
            page.body
        );
        let post_page = app.get(&format!("/posts/{post}"), None).await.body;
        assert!(
            post_page.contains(&format!("/posts/{post}/metadata")),
            "{post_page}"
        );
        assert_eq!(
            app.get("/posts/999999/metadata", None).await.status,
            StatusCode::NOT_FOUND
        );
    }
}
