//! What users show about themselves on their profile: a profile picture,
//! a banner and a bio. They edit them under Settings; staff can clear them.

use axum::extract::multipart::MultipartError;
use axum::extract::{DefaultBodyLimit, Multipart, Path};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::markup;
use moekura_core::moderation::ActionKind;
use moekura_db::mod_actions::{self, NewAction};
use moekura_db::users::{self, Profile};
use moekura_media::MediaType;
use moekura_storage::Key;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;

/// The largest picture accepted for either image.
const IMAGE_MAX_BYTES: usize = 10 * 1024 * 1024;
/// Profile pictures are cut square, at most this many pixels a side.
const AVATAR_SIZE: (u32, u32) = (400, 400);
/// Banners are cut to this shape, at most this size.
const BANNER_SIZE: (u32, u32) = (1500, 500);
/// The longest bio, in characters (the database allows as many).
pub(crate) const BIO_MAX_CHARS: usize = 4000;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/settings/profile",
            get(edit_form)
                .post(save)
                .layer(DefaultBodyLimit::max(2 * IMAGE_MAX_BYTES + 64 * 1024)),
        )
        .route("/users/{name}/profile/clear", post(clear))
}

/// `/users/<name>`.
pub(crate) fn profile_url(name: &str) -> String {
    format!(
        "/users/{}",
        url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>()
    )
}

/// Where browsers load a profile image from.
pub(crate) fn image_url(state: &AppState, key: Option<&str>) -> Option<Value> {
    let key = Key::parse(key?)?;
    Some(crate::templates::url_value(&state.file_url(&key)))
}

/// The profile's bio as HTML, if it has one.
pub(crate) fn bio_html(profile: &Profile) -> Option<Value> {
    (!profile.bio.trim().is_empty()).then(|| Value::from_safe_string(markup::render(&profile.bio)))
}

/// Users may change their profile unless banned.
fn editor(page: &Page) -> Result<&moekura_db::users::User, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    if page.current.ban.is_some() {
        return Err(AppError::Forbidden);
    }
    Ok(user)
}

async fn edit_form(page: Page) -> Result<Response, AppError> {
    let user = editor(&page)?;
    let profile = users::profile(page.state().db.primary(), user.id).await?;
    Ok(render_form(&page, &profile, &profile.bio, None))
}

fn render_form(page: &Page, profile: &Profile, bio: &str, error: Option<String>) -> Response {
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    let state = page.state();
    page.render_with_status(
        status,
        "profile_edit.html",
        context! {
            error => error,
            bio => bio,
            bio_max => BIO_MAX_CHARS,
            avatar => image_url(state, profile.avatar_key.as_deref()),
            banner => image_url(state, profile.banner_key.as_deref()),
            name => page.current.user.as_ref().map(|u| u.name.clone()),
            max_mb => IMAGE_MAX_BYTES / 1024 / 1024,
        },
    )
}

/// The edit form as sent.
#[derive(Default)]
struct ProfileForm {
    bio: Option<String>,
    avatar: Option<Vec<u8>>,
    banner: Option<Vec<u8>>,
    remove_avatar: bool,
    remove_banner: bool,
}

async fn read_form(mut multipart: Multipart) -> Result<ProfileForm, String> {
    let too_large = |e: MultipartError| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            format!(
                "Pictures can be at most {} MB.",
                IMAGE_MAX_BYTES / 1024 / 1024
            )
        } else {
            format!("The form was interrupted or malformed ({}).", e.body_text())
        }
    };
    let mut form = ProfileForm::default();
    while let Some(field) = multipart.next_field().await.map_err(too_large)? {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "avatar" | "banner" => {
                let bytes = field.bytes().await.map_err(too_large)?;
                if bytes.len() > IMAGE_MAX_BYTES {
                    return Err(format!(
                        "Pictures can be at most {} MB.",
                        IMAGE_MAX_BYTES / 1024 / 1024
                    ));
                }
                // Browsers send an empty part when no file was chosen.
                if !bytes.is_empty() {
                    let slot = if name == "avatar" {
                        &mut form.avatar
                    } else {
                        &mut form.banner
                    };
                    *slot = Some(bytes.to_vec());
                }
            }
            "bio" => {
                let text = crate::upload::text_field(field, 4 * BIO_MAX_CHARS).await;
                form.bio = Some(text.map_err(|error| match error {
                    crate::upload::TextFieldError::TooLong => {
                        format!("The bio can be at most {BIO_MAX_CHARS} characters.")
                    }
                    crate::upload::TextFieldError::Multipart(error) => too_large(error),
                })?);
            }
            "remove_avatar" => form.remove_avatar = true,
            "remove_banner" => form.remove_banner = true,
            _ => {}
        }
    }
    Ok(form)
}

async fn save(page: Page, jar: CookieJar, multipart: Multipart) -> Result<Response, AppError> {
    let user = editor(&page)?;
    let state = page.state();
    let db = state.db.primary();
    let before = users::profile(db, user.id).await?;
    let form = match read_form(multipart).await {
        Ok(form) => form,
        Err(error) => return Ok(render_form(&page, &before, &before.bio, Some(error))),
    };
    let bio = form
        .bio
        .map_or_else(|| before.bio.clone(), |b| b.replace("\r\n", "\n"))
        .trim()
        .to_owned();
    if bio.chars().count() > BIO_MAX_CHARS {
        let error = format!("The bio can be at most {BIO_MAX_CHARS} characters.");
        return Ok(render_form(&page, &before, &bio, Some(error)));
    }
    let mut after = Profile {
        bio,
        avatar_key: before.avatar_key.clone().filter(|_| !form.remove_avatar),
        banner_key: before.banner_key.clone().filter(|_| !form.remove_banner),
    };
    // Both pictures are made before either is stored, so a refused one
    // leaves nothing behind.
    let mut rendered = Vec::new();
    for (bytes, kind, size) in [
        (form.avatar, "avatar", AVATAR_SIZE),
        (form.banner, "banner", BANNER_SIZE),
    ] {
        let Some(bytes) = bytes else { continue };
        match render(state, &bytes, kind, size).await {
            Ok((key, picture)) => {
                let slot = if kind == "avatar" {
                    &mut after.avatar_key
                } else {
                    &mut after.banner_key
                };
                *slot = Some(key.as_str().to_owned());
                rendered.push((key, picture));
            }
            Err(ImageError::Invalid(error)) => {
                return Ok(render_form(&page, &before, &after.bio, Some(error)));
            }
            Err(ImageError::Internal(error)) => return Err(AppError::Internal(error)),
        }
    }
    // Removed again if the save fails.
    let mut stored = Vec::new();
    for (key, picture) in rendered {
        if let Err(error) = state.storage.put_bytes(&key, picture.into()).await {
            forget_unused(state, &stored).await;
            return Err(AppError::Internal(error.to_string()));
        }
        stored.push(key);
    }
    if let Err(error) = users::set_profile(db, user.id, &after).await {
        forget_unused(state, &stored).await;
        return Err(error.into());
    }
    forget_replaced(state, &before, &after).await;
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&profile_url(&user.name)),
    )
        .into_response())
}

enum ImageError {
    /// Something the user can fix, shown on the form.
    Invalid(String),
    Internal(String),
}

/// `bytes`, a picture someone sent, cut to `size` without its metadata,
/// and the key to store it under, under `kind/`.
async fn render(
    state: &AppState,
    bytes: &[u8],
    kind: &str,
    size: (u32, u32),
) -> Result<(Key, Vec<u8>), ImageError> {
    let token = hex::encode(moekura_core::tokens::NewToken::generate().hash);
    let source = TempFile(state.work_dir.join(format!("{kind}-{}", &token[..24])));
    tokio::fs::write(&source.0, bytes)
        .await
        .map_err(|e| ImageError::Internal(format!("writing temp file: {e}")))?;
    let invalid = |e: moekura_media::MediaError| {
        if e.is_internal() {
            ImageError::Internal(e.to_string())
        } else {
            ImageError::Invalid(format!("The picture can't be used: {e}."))
        }
    };
    let media_type = state.media.identify(&source.0).await.map_err(invalid)?;
    if media_type.is_video() || media_type == MediaType::Ugoira {
        return Err(ImageError::Invalid(
            "Choose a picture (JPEG, PNG, GIF, WebP or AVIF), not a video.".into(),
        ));
    }
    let probe = state
        .media
        .probe(&source.0, media_type)
        .await
        .map_err(invalid)?;
    let format = state.media.variant_format().to_owned();
    let out = TempFile(
        state
            .work_dir
            .join(format!("{kind}-{}.{format}", &token[24..48])),
    );
    state
        .media
        .cover(
            &source.0,
            media_type,
            (probe.width, probe.height),
            size,
            &out.0,
        )
        .await
        .map_err(invalid)?;
    let rendered = tokio::fs::read(&out.0)
        .await
        .map_err(|e| ImageError::Internal(format!("reading rendition: {e}")))?;
    let key = Key::variant(kind, &hex::encode(Sha256::digest(&rendered)), &format);
    Ok((key, rendered))
}

/// A file in the work directory, removed when dropped.
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Deletes the images `before` had that `after` doesn't, unless someone
/// else's profile uses the same file. Failing only leaves a file behind.
async fn forget_replaced(state: &AppState, before: &Profile, after: &Profile) {
    let kept = [after.avatar_key.as_deref(), after.banner_key.as_deref()];
    let old: Vec<Key> = [before.avatar_key.as_deref(), before.banner_key.as_deref()]
        .into_iter()
        .flatten()
        .filter(|old| !kept.contains(&Some(*old)))
        .filter_map(Key::parse)
        .collect();
    forget_unused(state, &old).await;
}

/// Deletes the images `keys` unless a profile uses them. Failing only
/// leaves a file behind.
async fn forget_unused(state: &AppState, keys: &[Key]) {
    for key in keys {
        match users::profile_image_used(state.db.primary(), key.as_str()).await {
            Ok(false) => {
                if let Err(error) = state.storage.delete(key).await {
                    tracing::warn!(%error, key = key.as_str(), "couldn't delete a profile image");
                }
            }
            Ok(true) => {}
            Err(error) => tracing::warn!(%error, "couldn't check a profile image's users"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ClearForm {
    // Checkboxes: present when ticked.
    avatar: Option<String>,
    banner: Option<String>,
    bio: Option<String>,
    #[serde(default)]
    reason: String,
}

/// Staff clear what someone put on their profile, for those they could
/// rename: the picture, the banner, the bio or all of them.
async fn clear(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(form): Form<ClearForm>,
) -> Result<Response, AppError> {
    let state = page.state();
    let db = state.db.primary();
    let user = users::by_name(db, &name).await?.ok_or(AppError::NotFound)?;
    if !crate::name_changes::may_rename(state, &page.current, &user) {
        return Err(AppError::Forbidden);
    }
    let before = users::profile(db, user.id).await?;
    let after = Profile {
        bio: if form.bio.is_some() {
            String::new()
        } else {
            before.bio.clone()
        },
        avatar_key: before.avatar_key.clone().filter(|_| form.avatar.is_none()),
        banner_key: before.banner_key.clone().filter(|_| form.banner.is_none()),
    };
    if after != before {
        let cleared: Vec<&str> = [
            ("avatar", before.avatar_key != after.avatar_key),
            ("banner", before.banner_key != after.banner_key),
            ("bio", before.bio != after.bio),
        ]
        .into_iter()
        .filter_map(|(what, changed)| changed.then_some(what))
        .collect();
        let mut tx = db.begin().await?;
        users::set_profile(&mut *tx, user.id, &after).await?;
        mod_actions::record(
            &mut *tx,
            NewAction::new(
                page.current.user.as_ref().map(|u| u.id),
                ActionKind::UserProfileClear,
            )
            .user(user.id)
            .reason(form.reason.trim())
            .details(serde_json::json!({ "cleared": cleared, "bio": before.bio })),
        )
        .await?;
        tx.commit().await?;
        forget_replaced(state, &before, &after).await;
    }
    Ok((
        flash::set(jar, Flash::Saved),
        Redirect::to(&profile_url(&user.name)),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use moekura_storage::Key;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, fixture, session_for, test_state};

    fn app(state: &crate::AppState) -> TestApp {
        TestApp::new(state.clone(), super::routes().merge(crate::users::routes()))
    }

    /// The `src` of the first image whose URL starts `prefix`.
    fn image_src<'a>(page: &'a str, prefix: &str) -> Option<&'a str> {
        let start = page.find(&format!("src=\"{prefix}"))? + 5;
        let end = start + page[start..].find('"')?;
        Some(&page[start..end])
    }

    /// How many files are stored under `dir`.
    fn files_under(dir: &std::path::Path) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        entries
            .flatten()
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() { files_under(&path) } else { 1 }
            })
            .sum()
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn refused_changes_keep_no_pictures(pool: PgPool) {
        let state = test_state(&pool).await;
        let app = app(&state);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let refused = app
            .post_multipart_files(
                "/settings/profile",
                Some(&alice),
                &[("bio", "hi".into())],
                &[
                    ("avatar", "me.png", &fixture::png(64, 64)),
                    ("banner", "x.svg", b"<svg/>"),
                ],
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        let stored = files_under(&state.config.storage.path.join("avatar"));
        assert_eq!(stored, 0, "the picture stored before the banner failed");
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn users_set_their_picture_banner_and_bio(pool: PgPool) {
        let state = test_state(&pool).await;
        let app = app(&state);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;

        assert_eq!(
            app.get("/settings/profile", None).await.location.as_deref(),
            Some("/login?next=%2Fsettings%2Fprofile")
        );
        let form = app.get("/settings/profile", Some(&alice)).await;
        assert_eq!(form.status, StatusCode::OK);
        assert!(form.body.contains("enctype=\"multipart/form-data\""));

        let (wide, tall) = (fixture::png(900, 300), fixture::png(300, 900));
        let saved = app
            .post_multipart_files(
                "/settings/profile",
                Some(&alice),
                &[("bio", "Draws [b]cats[/b].\r\n".into())],
                &[("avatar", "me.png", &wide), ("banner", "b.png", &tall)],
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        assert_eq!(saved.location.as_deref(), Some("/users/alice"));

        let profile = app.get("/users/alice", None).await.body;
        assert!(profile.contains("<strong>cats</strong>"), "{profile}");
        let avatar = image_src(&profile, "/data/avatar/")
            .expect(&profile)
            .to_owned();
        let banner = image_src(&profile, "/data/banner/")
            .expect(&profile)
            .to_owned();
        let file = app.get_full(&avatar).await;
        assert_eq!(file.status(), StatusCode::OK);
        // Cut square and to 3:1, at most the source's size.
        let size = |url: &str| {
            let path = state
                .config
                .storage
                .path
                .join(url.trim_start_matches("/data/"));
            let field = |name: &str| -> u32 {
                let out = std::process::Command::new("vipsheader")
                    .args(["-f", name])
                    .arg(&path)
                    .output()
                    .unwrap();
                String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
            };
            (field("width"), field("height"))
        };
        assert_eq!(size(&avatar), (300, 300));
        assert_eq!(size(&banner), (300, 100));

        // Other files are turned away, and so is too long a bio.
        let refused = app
            .post_multipart_files(
                "/settings/profile",
                Some(&alice),
                &[("bio", "still here".into())],
                &[("avatar", "x.svg", b"<svg/>")],
            )
            .await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            refused.body.contains(">still here</textarea>"),
            "{}",
            refused.body
        );
        let long = app
            .post_multipart(
                "/settings/profile",
                Some(&alice),
                &[("bio", "a".repeat(super::BIO_MAX_CHARS + 1))],
                None,
            )
            .await;
        assert_eq!(long.status, StatusCode::UNPROCESSABLE_ENTITY);

        // Removing the picture deletes its file; the banner stays.
        let removed = app
            .post_multipart(
                "/settings/profile",
                Some(&alice),
                &[("bio", String::new()), ("remove_avatar", "1".into())],
                None,
            )
            .await;
        assert_eq!(removed.status, StatusCode::SEE_OTHER);
        let profile = app.get("/users/alice", None).await.body;
        assert!(image_src(&profile, "/data/avatar/").is_none());
        assert!(image_src(&profile, "/data/banner/").is_some());
        assert!(!profile.contains("profile-bio"));
        let key = Key::parse(avatar.trim_start_matches("/data/")).unwrap();
        assert!(!state.storage.exists(&key).await.unwrap());

        // Banned users can't change theirs.
        sqlx::query(
            "INSERT INTO bans (user_id, reason) SELECT id, 'spam' FROM users WHERE name = 'alice'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let banned = app
            .post_multipart(
                "/settings/profile",
                Some(&alice),
                &[("bio", "hi".into())],
                None,
            )
            .await;
        assert_eq!(banned.status, StatusCode::FORBIDDEN);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn staff_clear_profiles(pool: PgPool) {
        let state = test_state(&pool).await;
        let app = app(&state);
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let moderator = session_for(&pool, "mod", SystemRole::Moderator).await;
        let png = fixture::png(64, 64);
        app.post_multipart_files(
            "/settings/profile",
            Some(&alice),
            &[("bio", "Buy followers at spam.example".into())],
            &[("avatar", "a.png", &png)],
        )
        .await;

        // Only staff see the form, and only they may use it.
        assert!(
            !app.get("/users/alice", Some(&bob))
                .await
                .body
                .contains("/profile/clear")
        );
        let page = app.get("/users/alice", Some(&moderator)).await.body;
        assert!(page.contains("/users/alice/profile/clear"), "{page}");
        let forbidden = app
            .post_form("/users/alice/profile/clear", Some(&bob), &[], "bio=1")
            .await;
        assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

        let cleared = app
            .post_form(
                "/users/alice/profile/clear",
                Some(&moderator),
                &[],
                "bio=1&reason=spam",
            )
            .await;
        assert_eq!(cleared.status, StatusCode::SEE_OTHER);
        let profile = profile_of(&pool, "alice").await;
        assert!(profile.bio.is_empty());
        assert!(profile.avatar_key.is_some(), "only what was ticked");
        let (reason, details): (String, serde_json::Value) = sqlx::query_as(
            "SELECT reason, details FROM mod_actions WHERE action = 'user.profile_clear'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(reason, "spam");
        assert_eq!(details["cleared"], serde_json::json!(["bio"]));
        assert_eq!(details["bio"], "Buy followers at spam.example");
    }

    async fn profile_of(pool: &PgPool, name: &str) -> moekura_db::users::Profile {
        let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
        moekura_db::users::profile(pool, id).await.unwrap()
    }
}
