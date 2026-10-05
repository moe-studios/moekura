//! Replacing a post's file with a better version (`/posts/{id}/replace`),
//! keeping everything else about the post, and the list of a post's
//! replacements.

use axum::Router;
use axum::extract::{Multipart, Path};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::CookieJar;
use minijinja::context;
use moekura_core::jobs::ProcessMedia;
use moekura_core::moderation::ActionKind;
use moekura_core::notes::NoteBox;
use moekura_core::permissions::Permission;
use moekura_db::media::NewAsset;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::replacements::{self, ReplaceError};
use moekura_db::{notes, posts};
use moekura_storage::Key;

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::upload::{
    LINK_FIELD_MAX, TEXT_FIELD_MAX, TempUpload, UploadError, UploadFields, text_field,
};

pub fn routes(max_upload_bytes: u64) -> Router<AppState> {
    Router::new()
        .route(
            "/posts/{id}/replace",
            post(replace).layer(crate::upload::body_limit(max_upload_bytes)),
        )
        .route("/posts/{id}/replacements", get(list))
}

/// What to replace a post's file with.
pub(crate) struct Replacement<'a> {
    pub file: &'a TempUpload,
    pub reason: &'a str,
    /// The link the file came from, if any.
    pub source: &'a str,
    /// Move and resize notes to fit the new size.
    pub rescale_notes: bool,
}

fn refused(error: UploadError) -> AppError {
    match error {
        UploadError::Duplicate(id) => AppError::Duplicate(id),
        UploadError::TooFast(retry_after_secs) => AppError::TooManyRequests { retry_after_secs },
        UploadError::Internal(detail) => AppError::Internal(detail),
        other => AppError::Unprocessable(other.to_string()),
    }
}

/// Replaces post `post_id`'s file as `current`, who must be allowed to.
pub(crate) async fn replace_file(
    state: &AppState,
    current: &CurrentUser,
    post_id: i64,
    replacement: Replacement<'_>,
) -> Result<(), AppError> {
    current.require(Permission::ReplacePosts)?;
    let db = state.db.primary();
    let post = posts::by_id(db, post_id)
        .await?
        .filter(|p| crate::posts::visibility(current).allows(p))
        .ok_or(AppError::NotFound)?;
    if replacement.reason.chars().count() > 1000 {
        return Err(AppError::Unprocessable(
            "The reason can be at most 1000 characters long.".into(),
        ));
    }
    let prepared = match crate::upload::prepare(state, Some(current), replacement.file).await {
        Ok(prepared) => prepared,
        Err(UploadError::Duplicate(id)) if id == post.id => {
            return Err(AppError::Unprocessable(
                "That's the file the post already has.".into(),
            ));
        }
        Err(error) => return Err(refused(error)),
    };
    let replaced = record(state, current, &post, &replacement, &prepared).await;
    if replaced.is_err() {
        crate::upload::forget_original(state, &prepared).await;
    }
    replaced
}

/// The rest of [`replace_file`], once the new file is stored.
async fn record(
    state: &AppState,
    current: &CurrentUser,
    post: &posts::Post,
    replacement: &Replacement<'_>,
    prepared: &crate::upload::Prepared,
) -> Result<(), AppError> {
    let db = state.db.primary();
    let new = NewAsset {
        post_id: post.id,
        sha256: &prepared.sha256,
        md5: &prepared.md5,
        media_type: &prepared.media_type,
        width: prepared.width,
        height: prepared.height,
        duration_ms: prepared.duration_ms,
        frames: prepared.frames,
        has_audio: prepared.has_audio,
        file_size: prepared.file_size,
        storage_key: &prepared.storage_key,
    };
    let actor = current.user.as_ref().map(|u| u.id);
    let mut tx = db.begin().await?;
    crate::upload::keep_original(state, &mut tx, prepared)
        .await
        .map_err(refused)?;
    let replaced = match replacements::replace(
        &mut tx,
        &new,
        actor,
        replacement.reason.trim(),
        replacement.source,
    )
    .await
    {
        Ok(replaced) => replaced,
        Err(ReplaceError::NoFile) => return Err(AppError::NotFound),
        Err(ReplaceError::Same) => {
            return Err(AppError::Unprocessable(
                "That's the file the post already has.".into(),
            ));
        }
        Err(ReplaceError::Duplicate(id)) => {
            drop(tx);
            return Err(refused(
                crate::upload::duplicate(state, Some(current), id).await,
            ));
        }
        Err(ReplaceError::Db(e)) => return Err(e.into()),
    };
    moekura_db::media::set_facts(
        &mut *tx,
        replaced.old.id,
        prepared.pixel_hash.as_ref(),
        &prepared.traits,
    )
    .await?;
    moekura_db::jobs::enqueue(
        &mut tx,
        &ProcessMedia {
            asset_id: replaced.old.id,
        },
    )
    .await?;
    let reason = replacement.reason.trim();
    mod_actions::record(
        &mut *tx,
        NewAction::new(actor, ActionKind::PostReplace)
            .post(post.id)
            .reason(reason)
            .details(serde_json::json!({
                "from": format!("{}×{} {}", replaced.old.width, replaced.old.height, replaced.old.media_type),
                "to": format!("{}×{} {}", new.width, new.height, new.media_type),
            })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(post_id = post.id, "post file replaced");

    // The old renditions are no use now; the old original stays, for the
    // post's replacement history.
    for key in replaced
        .old_variant_keys
        .iter()
        .filter_map(|k| Key::parse(k))
    {
        if let Err(error) = state.storage.delete(&key).await {
            tracing::warn!(%error, key = %key, "could not delete an old rendition");
        }
    }
    if replacement.rescale_notes
        && (replaced.old.width, replaced.old.height) != (new.width, new.height)
    {
        let sx = f64::from(new.width) / f64::from(replaced.old.width.max(1));
        let sy = f64::from(new.height) / f64::from(replaced.old.height.max(1));
        for note in notes::for_post(db, post.id, false).await? {
            let scaled = NoteBox {
                x: (f64::from(note.x) * sx).round() as i32,
                y: (f64::from(note.y) * sy).round() as i32,
                width: ((f64::from(note.width) * sx).round() as i32).max(1),
                height: ((f64::from(note.height) * sy).round() as i32).max(1),
            };
            let changes = notes::Changes {
                note_box: Some(scaled),
                ..notes::Changes::default()
            };
            if let Err(error) = notes::update(db, note.id, &changes, actor, None).await {
                tracing::warn!(%error, note = note.id, "could not rescale a note");
            }
        }
    }
    Ok(())
}

async fn replace(
    page: Page,
    jar: CookieJar,
    Path(id): Path<i64>,
    mut form: Multipart,
) -> Result<Response, AppError> {
    page.current.require(Permission::ReplacePosts)?;
    let state = page.state();
    // A new file is processed like an upload, and counted as one.
    crate::upload::check_pace(state, &page.current)
        .await
        .map_err(refused)?;
    let mut file = None;
    let mut fields = UploadFields::default();
    let mut reason = String::new();
    let mut rescale = false;
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(e.body_text()))?
    {
        match field.name().unwrap_or_default() {
            "file" => {
                if field.file_name().is_none_or(str::is_empty) {
                    continue;
                }
                file = Some(
                    crate::upload::save_to_temp(state, field)
                        .await
                        .map_err(refused)?,
                );
            }
            "url" => {
                fields.url = text_field(field, LINK_FIELD_MAX).await?.trim().to_owned();
            }
            "reason" => reason = text_field(field, TEXT_FIELD_MAX).await?,
            "rescale_notes" => rescale = true,
            _ => {}
        }
    }
    let file = match file {
        Some(file) => file,
        None if !fields.url.is_empty() => crate::upload::fetch_url(state, &mut fields)
            .await
            .map_err(refused)?,
        None => {
            return Err(AppError::Unprocessable(
                "Choose the new file, or paste a link to it.".into(),
            ));
        }
    };
    replace_file(
        state,
        &page.current,
        id,
        Replacement {
            file: &file,
            reason: &reason,
            source: &fields.source,
            rescale_notes: rescale,
        },
    )
    .await?;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&format!("/posts/{id}")),
    )
        .into_response())
}

async fn list(page: Page, Path(id): Path<i64>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let state = page.state();
    let db = state.reader(&page.current);
    posts::by_id(db, id)
        .await?
        .filter(|p| crate::posts::visibility(&page.current).allows(p))
        .ok_or(AppError::NotFound)?;
    let found = replacements::list(
        db,
        replacements::Filter {
            post_id: Some(id),
            ..replacements::Filter::default()
        },
        0,
        200,
    )
    .await?;
    let file =
        |key: &str| Key::parse(key).map(|k| crate::templates::url_value(&state.file_url(&k)));
    let human = crate::posts::human_size;
    Ok(page.render(
        "post_replacements.html",
        context! {
            post_id => id,
            replacements => found.iter().map(|r| context! {
                date => crate::dates::day(r.created_at),
                time => crate::dates::clock(r.created_at),
                creator => r.creator_name,
                reason => r.reason,
                source => r.source,
                old => context! {
                    url => file(&r.old_storage_key),
                    size => format!("{}×{}", r.old_width, r.old_height),
                    kind => r.old_media_type.to_uppercase(),
                    bytes => human(r.old_file_size),
                    md5 => hex::encode(&r.old_md5),
                },
                new => context! {
                    size => format!("{}×{}", r.new_width, r.new_height),
                    kind => r.new_media_type.to_uppercase(),
                    bytes => human(r.new_file_size),
                    md5 => hex::encode(&r.new_md5),
                },
            }).collect::<Vec<_>>(),
        },
    ))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_db::media;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn replacing_is_uploading(pool: PgPool) {
        use sha2::{Digest, Sha256};
        // Members who may replace posts, but not approve them.
        sqlx::query(
            "UPDATE roles SET permissions = permissions | (1::bigint << 24)
             WHERE system_key = 'member'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = test_state(&pool).await;
        let app = TestApp::new(
            state.clone(),
            super::routes(1024 * 1024 * 10).merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let post = crate::danbooru::test_support::upload(&app, &alice, 20, "cat").await;
        let url = format!("/posts/{post}/replace");

        // Another's deleted post isn't named.
        let theirs = crate::danbooru::test_support::upload(&app, &bob, 30, "dog").await;
        sqlx::query("UPDATE posts SET status = 'deleted' WHERE id = $1")
            .bind(theirs)
            .execute(&pool)
            .await
            .unwrap();
        let taken = fixture::png(30, 20);
        let refused = app
            .post_multipart(&url, Some(&alice), &[], Some(("b.png", taken.as_slice())))
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(!refused.body.contains(&format!("/posts/{theirs}")));

        // Each new file counts as an upload.
        let id = crate::test_support::current_user(&state, &alice)
            .await
            .user
            .unwrap()
            .id;
        while state.rate_limits.check_upload(id).await.is_ok() {}
        let bigger = fixture::png(40, 40);
        let limited = app
            .post_multipart(&url, Some(&alice), &[], Some(("c.png", bigger.as_slice())))
            .await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        let key = moekura_storage::Key::original(&hex::encode(Sha256::digest(&bigger)), "png");
        assert!(!state.storage.exists(&key).await.unwrap());
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn replaces_files(pool: PgPool) {
        let state = test_state(&pool).await;
        let app = TestApp::new(
            state.clone(),
            super::routes(1024 * 1024 * 10).merge(crate::danbooru::test_support::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let boss = session_for(&pool, "boss", SystemRole::Moderator).await;
        let post = crate::danbooru::test_support::upload(&app, &alice, 20, "cat").await;
        let other = crate::danbooru::test_support::upload(&app, &alice, 30, "dog").await;
        moekura_db::notes::create(
            &pool,
            post,
            moekura_core::notes::NoteBox {
                x: 2,
                y: 4,
                width: 10,
                height: 10,
            },
            "hi",
            None,
        )
        .await
        .unwrap();

        let url = format!("/posts/{post}/replace");
        let bigger = fixture::png(40, 40);
        let file = Some(("b.png", bigger.as_slice()));
        let reason = [
            ("reason", "Higher resolution".to_owned()),
            ("rescale_notes", "1".to_owned()),
        ];
        let refused = app.post_multipart(&url, Some(&alice), &reason, file).await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        let done = app.post_multipart(&url, Some(&boss), &reason, file).await;
        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.body);

        let asset = media::for_post(&pool, post).await.unwrap().unwrap();
        assert_eq!((asset.width, asset.height), (40, 40));
        assert!(asset.processed_at.is_none(), "processed again");
        // 20×20 to 40×40: notes double.
        let note = &moekura_db::notes::for_post(&pool, post, false)
            .await
            .unwrap()[0];
        assert_eq!((note.x, note.y, note.width, note.height), (4, 8, 20, 20));

        let same = app.post_multipart(&url, Some(&boss), &[], file).await;
        assert_eq!(same.status, StatusCode::UNPROCESSABLE_ENTITY);
        let taken = fixture::png(30, 20);
        let duplicate = app
            .post_multipart(&url, Some(&boss), &[], Some(("c.png", taken.as_slice())))
            .await;
        assert_eq!(duplicate.status, StatusCode::CONFLICT, "{}", duplicate.body);
        let _ = other;

        let history = app
            .get(&format!("/posts/{post}/replacements"), None)
            .await
            .body;
        assert!(
            history.contains("Higher resolution") && history.contains("20×20"),
            "{history}"
        );
        let logged: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM mod_actions WHERE action = 'post.replace' AND post_id = $1",
        )
        .bind(post)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(logged, 1);
        let listed = app
            .get(
                &format!("/post_replacements.json?search[post_id]={post}"),
                None,
            )
            .await
            .body;
        assert!(listed.contains("\"image_width_was\":20"), "{listed}");
    }
}
