//! Choosing the region a post's square thumbnails show
//! (`/posts/{id}/crop`), for staff who review posts.

use axum::extract::Path;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use moekura_core::jobs::ProcessMedia;
use moekura_core::permissions::Permission;
use moekura_db::{media, posts};
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

pub fn routes() -> Router<AppState> {
    Router::new().route("/posts/{id}/crop", post(set_crop))
}

#[derive(Debug, Deserialize)]
struct CropForm {
    #[serde(default)]
    left: String,
    #[serde(default)]
    top: String,
    #[serde(default)]
    side: String,
    /// Present to go back to the automatic crop.
    reset: Option<String>,
}

/// A crop within a `width`×`height` file, from the form's numbers.
fn checked(form: &CropForm, width: i32, height: i32) -> Result<[i32; 3], AppError> {
    let number = |text: &str| text.trim().parse::<i32>().ok();
    let (Some(left), Some(top), Some(side)) =
        (number(&form.left), number(&form.top), number(&form.side))
    else {
        return Err(AppError::Unprocessable(
            "Give the square's left and top edges and its side, in pixels.".into(),
        ));
    };
    if left < 0 || top < 0 || side < 1 || left + side > width || top + side > height {
        return Err(AppError::Unprocessable(format!(
            "The square must fit in the {width}×{height} picture."
        )));
    }
    Ok([left, top, side])
}

async fn set_crop(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    Form(form): Form<CropForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ApprovePosts)?;
    let db = page.state().db.primary();
    posts::by_id(db, id)
        .await?
        .filter(|p| crate::posts::visibility(&page.current).allows(p))
        .ok_or(AppError::NotFound)?;
    let asset = media::for_post(db, id).await?.ok_or(AppError::NotFound)?;
    let crop = match form.reset {
        Some(_) => None,
        None => Some(checked(&form, asset.width, asset.height)?),
    };
    let mut tx = db.begin().await?;
    media::set_crop(&mut *tx, id, crop).await?;
    moekura_db::jobs::enqueue(&mut tx, &ProcessMedia { asset_id: asset.id }).await?;
    tx.commit().await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/posts/{id}")),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn staff_choose_the_crop(pool: PgPool) {
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let boss = session_for(&pool, "boss", SystemRole::Janitor).await;
        // 40×20.
        let post = crate::danbooru::test_support::upload(&app, &alice, 40, "cat").await;
        let url = format!("/posts/{post}/crop");
        assert_eq!(
            app.post_form(&url, Some(&alice), &[], "left=0&top=0&side=10")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let outside = app
            .post_form(&url, Some(&boss), &[], "left=30&top=0&side=20")
            .await;
        assert_eq!(outside.status, StatusCode::UNPROCESSABLE_ENTITY);
        let done = app
            .post_form(&url, Some(&boss), &[], "left=20&top=0&side=20")
            .await;
        assert_eq!(done.status, StatusCode::SEE_OTHER);
        let asset = moekura_db::media::for_post(&pool, post)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            moekura_db::media::crop(&pool, asset.id).await.unwrap(),
            Some([20, 0, 20])
        );
        app.post_form(&url, Some(&boss), &[], "reset=1").await;
        assert_eq!(
            moekura_db::media::crop(&pool, asset.id).await.unwrap(),
            None
        );
    }
}
