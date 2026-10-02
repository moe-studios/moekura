//! Admin → Tag categories: adding, renaming, reordering and deleting the
//! groups tags are sorted into.
//!
//! Ids never change: posts' per-category counts, tag history and the
//! Danbooru-compatible API refer to categories by id. Danbooru's own
//! categories keep their names too (imports and Danbooru apps rely on
//! them), so only their labels and order can change, and they can't be
//! deleted. Categories are read from the database on every use, so every
//! node sees a change at once.

use axum::extract::{Form, Path};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Router, http::StatusCode};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::moderation::ActionKind;
use moekura_core::permissions::Permission;
use moekura_core::tags::{check_category_label, check_category_name, is_built_in_category};
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::tags::{self, Category};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

const PATH: &str = "/admin/tag-categories";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(PATH, get(list))
        .route("/admin/tag-categories/new", post(create))
        .route("/admin/tag-categories/{id}", post(update))
        .route("/admin/tag-categories/{id}/move", post(move_category))
        .route("/admin/tag-categories/{id}/delete", post(delete))
}

fn saved(jar: CookieJar, to: &str) -> Response {
    use axum::response::IntoResponse;
    (
        flash::set(jar, Flash::Saved),
        axum::response::Redirect::to(to),
    )
        .into_response()
}

fn actor(page: &Page) -> Option<i64> {
    page.current.user.as_ref().map(|u| u.id)
}

async fn render(page: &Page, error: Option<String>) -> Result<Response, AppError> {
    let db = page.state().db.primary();
    let categories = tags::categories(db).await?;
    let mut rows: Vec<Value> = Vec::with_capacity(categories.len());
    for (i, category) in categories.iter().enumerate() {
        let built_in = is_built_in_category(category.id);
        let tag_count = tags::count_in_category(db, category.id).await?;
        rows.push(context! {
            id => category.id,
            name => category.name,
            label => category.label,
            built_in => built_in,
            tag_count => tag_count,
            deletable => !built_in && tag_count == 0,
            first => i == 0,
            last => i + 1 == categories.len(),
        });
    }
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    Ok(page.render_with_status(
        status,
        "admin_tag_categories.html",
        context! {
            categories => rows,
            error => error,
            name_max => moekura_core::tags::CATEGORY_NAME_MAX_LEN,
            label_max => moekura_core::tags::CATEGORY_LABEL_MAX_LEN,
        },
    ))
}

async fn list(page: Page) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    render(&page, None).await
}

#[derive(Debug, Deserialize)]
struct CategoryForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    label: String,
}

/// Why `name` can't be a category's: the rules, then whether tags named
/// `name:…` exist, which would become unreachable from tag boxes.
async fn check_name(state: &AppState, name: &str) -> Result<Result<(), String>, AppError> {
    if let Err(message) = check_category_name(name) {
        return Ok(Err(message));
    }
    if tags::any_with_prefix(state.db.primary(), name).await? {
        return Ok(Err(format!(
            "Tags named `{name}:…` exist; rename them before using `{name}` as a category"
        )));
    }
    Ok(Ok(()))
}

fn name_taken(error: sqlx::Error) -> Result<(), AppError> {
    match &error {
        sqlx::Error::Database(d) if d.is_unique_violation() => Ok(()),
        _ => Err(error.into()),
    }
}

async fn create(
    page: Page,
    jar: CookieJar,
    Form(form): Form<CategoryForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let name = form.name.trim();
    let label = form.label.trim();
    if let Err(message) = check_name(state, name)
        .await?
        .and(check_category_label(label))
    {
        return render(&page, Some(message)).await;
    }
    let mut tx = state.db.primary().begin().await?;
    let category = match tags::create_category(&mut tx, name, label).await {
        Ok(category) => category,
        Err(error) => {
            name_taken(error)?;
            return render(&page, Some(format!("There's already a category `{name}`"))).await;
        }
    };
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), ActionKind::TagCategoryCreate).details(json!({
            "id": category.id,
            "name": category.name,
            "label": category.label,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(saved(jar, &format!("{PATH}#category-{}", category.id)))
}

async fn existing(state: &AppState, id: i16) -> Result<Category, AppError> {
    tags::category(state.db.primary(), id)
        .await?
        .ok_or(AppError::NotFound)
}

async fn update(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i16>,
    Form(form): Form<CategoryForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let before = existing(state, id).await?;
    let label = form.label.trim();
    // Danbooru's categories keep their names.
    let name = if is_built_in_category(id) || form.name.trim().is_empty() {
        before.name.as_str()
    } else {
        form.name.trim()
    };
    let name_ok = if name == before.name {
        Ok(())
    } else {
        check_name(state, name).await?
    };
    if let Err(message) = name_ok.and(check_category_label(label)) {
        return render(&page, Some(message)).await;
    }
    if name == before.name && label == before.label {
        return Ok(saved(jar, &format!("{PATH}#category-{id}")));
    }
    let mut tx = state.db.primary().begin().await?;
    if let Err(error) = tags::rename_category(&mut *tx, id, name, label).await {
        name_taken(error)?;
        return render(&page, Some(format!("There's already a category `{name}`"))).await;
    }
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), ActionKind::TagCategoryUpdate).details(json!({
            "id": id,
            "from": format!("{} ({})", before.name, before.label),
            "to": format!("{name} ({label})"),
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(saved(jar, &format!("{PATH}#category-{id}")))
}

#[derive(Debug, Deserialize)]
struct MoveForm {
    /// `up` or `down`.
    direction: String,
}

async fn move_category(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i16>,
    Form(form): Form<MoveForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let category = existing(state, id).await?;
    let up = match form.direction.as_str() {
        "up" => true,
        "down" => false,
        _ => return Err(AppError::BadRequest("Move a category up or down".into())),
    };
    let mut tx = state.db.primary().begin().await?;
    if tags::move_category(&mut tx, id, up).await? {
        mod_actions::record(
            &mut *tx,
            NewAction::new(actor(&page), ActionKind::TagCategoryMove).details(json!({
                "category": category.name,
                "direction": form.direction,
            })),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(saved(jar, &format!("{PATH}#category-{id}")))
}

async fn delete(page: Page, jar: CookieJar, Path(id): Path<i16>) -> Result<Response, AppError> {
    page.current.require(Permission::ManageSettings)?;
    let state = page.state();
    let category = existing(state, id).await?;
    if is_built_in_category(id) {
        return render(
            &page,
            Some(format!(
                "`{}` is one of Danbooru's categories and stays",
                category.name
            )),
        )
        .await;
    }
    let mut tx = state.db.primary().begin().await?;
    if !tags::delete_category(&mut *tx, id).await? {
        return render(
            &page,
            Some(format!(
                "Tags are still in `{}`; move them to another category first",
                category.name
            )),
        )
        .await;
    }
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor(&page), ActionKind::TagCategoryDelete).details(json!({
            "id": id,
            "name": category.name,
            "label": category.label,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(saved(jar, PATH))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    async fn app(pool: &PgPool) -> TestApp {
        let state = test_state(pool).await;
        TestApp::new(state, super::routes().merge(crate::tags::routes()))
    }

    async fn names(pool: &PgPool) -> Vec<String> {
        moekura_db::tags::categories(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    async fn logged(pool: &PgPool) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT action FROM mod_actions WHERE action LIKE 'tag_category.%' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn admins_manage_categories(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        assert_eq!(
            app.get("/admin/tag-categories", Some(&moderator))
                .await
                .status,
            StatusCode::FORBIDDEN
        );
        let page = app.get("/admin/tag-categories", Some(&admin)).await;
        assert_eq!(page.status, StatusCode::OK);
        assert!(page.body.contains("id=\"category-1\""), "{}", page.body);

        let added = app
            .post_form(
                "/admin/tag-categories/new",
                Some(&admin),
                &[],
                "name=species&label=Species",
            )
            .await;
        assert_eq!(added.status, StatusCode::SEE_OTHER, "{}", added.body);
        assert_eq!(
            added.location.as_deref(),
            Some("/admin/tag-categories#category-6")
        );
        for bad in [
            "name=species&label=Again",
            "name=rating&label=R",
            "name=Bad&label=B",
            "name=lore&label=",
        ] {
            let refused = app
                .post_form("/admin/tag-categories/new", Some(&admin), &[], bad)
                .await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        }

        // Up past meta; Danbooru's keep their names but take new labels.
        let moved = app
            .post_form(
                "/admin/tag-categories/6/move",
                Some(&admin),
                &[],
                "direction=up",
            )
            .await;
        assert_eq!(moved.status, StatusCode::SEE_OTHER);
        assert_eq!(
            names(&pool).await,
            [
                "artist",
                "copyright",
                "character",
                "general",
                "species",
                "meta"
            ]
        );
        let relabelled = app
            .post_form(
                "/admin/tag-categories/1",
                Some(&admin),
                &[],
                "name=creator&label=Creator",
            )
            .await;
        assert_eq!(relabelled.status, StatusCode::SEE_OTHER);
        let artist = moekura_db::tags::category(&pool, 1).await.unwrap().unwrap();
        assert_eq!(
            (artist.name.as_str(), artist.label.as_str()),
            ("artist", "Creator")
        );
        let renamed = app
            .post_form(
                "/admin/tag-categories/6",
                Some(&admin),
                &[],
                "name=animal&label=Animal",
            )
            .await;
        assert_eq!(renamed.status, StatusCode::SEE_OTHER);

        // A category with tags stays; built-in ones always do.
        sqlx::query("INSERT INTO tags (name, category_id) VALUES ('cat', 6)")
            .execute(&pool)
            .await
            .unwrap();
        let kept = app
            .post("/admin/tag-categories/6/delete", Some(&admin), &[])
            .await;
        assert_eq!(kept.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(kept.body.contains("Tags are still in"), "{}", kept.body);
        let built_in = app
            .post("/admin/tag-categories/5/delete", Some(&admin), &[])
            .await;
        assert_eq!(built_in.status, StatusCode::UNPROCESSABLE_ENTITY);
        sqlx::query("UPDATE tags SET category_id = 0")
            .execute(&pool)
            .await
            .unwrap();
        let deleted = app
            .post("/admin/tag-categories/6/delete", Some(&admin), &[])
            .await;
        assert_eq!(deleted.status, StatusCode::SEE_OTHER);
        assert_eq!(names(&pool).await.len(), 5);
        assert_eq!(
            logged(&pool).await,
            [
                "tag_category.create",
                "tag_category.move",
                "tag_category.update",
                "tag_category.update",
                "tag_category.delete"
            ]
        );
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn names_tags_already_use_are_refused(pool: PgPool) {
        let app = app(&pool).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        sqlx::query("INSERT INTO tags (name) VALUES ('lore:old_tag')")
            .execute(&pool)
            .await
            .unwrap();
        let refused = app
            .post_form(
                "/admin/tag-categories/new",
                Some(&admin),
                &[],
                "name=lore&label=Lore",
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("lore:…"), "{}", refused.body);
    }
}
