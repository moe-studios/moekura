//! User profiles and the logged-in user's settings.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use axum_extra::extract::CookieJar;
use minijinja::{Value, context};
use moekura_core::blacklist::Blacklist;
use moekura_core::permissions::Permission;
use moekura_core::user_settings::{MAX_CUSTOM_CSS, Mode, PER_PAGE_CHOICES, UserSettings};
use moekura_db::users::{self, UserStatus};
use moekura_db::{favorites, posts};
use serde::Deserialize;

use crate::AppState;
use crate::error::AppError;
use crate::flash::{self, Flash};
use crate::pages::Page;
use crate::templates::search_url;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users/{name}", get(profile))
        .route(
            "/users/{name}/auto-promotion",
            axum::routing::post(set_auto_promotion),
        )
        .route("/settings", get(settings_form).post(save_settings))
        .route("/settings/theme", axum::routing::post(set_theme))
        .route("/settings/custom.css", get(custom_css))
}

async fn profile(page: Page, Path(name): Path<String>) -> Result<Response, AppError> {
    page.current.require(Permission::ViewPosts)?;
    let db = page.state().reader(&page.current);
    let Some(user) = users::by_name(db, &name).await? else {
        // Someone who has since changed their name.
        return match users::by_name_or_former(page.state().db.primary(), &name).await? {
            Some(user) => Ok(Redirect::permanent(&format!(
                "/users/{}",
                url::form_urlencoded::byte_serialize(user.name.as_bytes()).collect::<String>()
            ))
            .into_response()),
            None => Err(AppError::NotFound),
        };
    };
    if user.status != UserStatus::Active && !page.current.can(Permission::ManageUsers) {
        return Err(AppError::NotFound);
    }
    let site = page.state().site.get();
    let role = site.role(user.role_id).map(|r| r.name.clone());
    let uploads = posts::count_by_uploader(db, user.id).await?;
    let staff =
        page.current.can(Permission::BanUsers) || page.current.can(Permission::ViewAuditLog);
    let ban_history = if staff {
        moekura_db::bans::for_user(db, user.id).await?
    } else {
        Vec::new()
    };
    let can_ban = crate::bans::may_ban(page.state(), &page.current, &user);
    let banned = ban_history.iter().any(|b| b.active);
    let favorites = favorites::count_by_user(db, user.id).await?;
    let comments = moekura_db::comments::count_by_user(db, user.id).await?;
    let (promotion_blocked, promoted_at) = moekura_db::promotion::status(db, user.id).await?;
    let can_manage = page.current.can(Permission::ManageUsers)
        && page
            .state()
            .site
            .get()
            .role(user.role_id)
            .is_some_and(|r| page.current.role.outranks(r));
    let own = page.current.user.as_ref().map(|u| u.id) == Some(user.id);
    let favorite_groups = moekura_db::favorite_groups::for_user(db, user.id, own)
        .await?
        .len();
    let notes = crate::user_moderation::notes(page.state(), &page.current, user.id).await?;
    let me = page.current.user.as_ref().map(|u| u.id);
    let messages = match me.filter(|&me| me != user.id) {
        Some(me) => Some(context! {
            can_send => page.current.can(Permission::SendMessages),
            blocked => moekura_db::dmails::is_blocked(db, me, user.id).await?,
        }),
        None => None,
    };
    let former_names: Vec<String> = moekura_db::name_changes::for_user(db, user.id)
        .await?
        .into_iter()
        .map(|c| c.old_name)
        .filter(|old| !old.eq_ignore_ascii_case(&user.name))
        .fold(Vec::new(), |mut names, old| {
            if !names.iter().any(|n: &String| n.eq_ignore_ascii_case(&old)) {
                names.push(old);
            }
            names
        });
    let can_rename = crate::name_changes::may_rename(page.state(), &page.current, &user);
    let invited_by = moekura_db::invites::inviter(db, user.id)
        .await?
        .map(|(_, name)| name);
    let [positive, neutral, negative] = moekura_db::user_feedbacks::counts(db, user.id).await?;
    let feedback = context! {
        positive => positive,
        neutral => neutral,
        negative => negative,
        url => crate::templates::url_value(&crate::user_feedbacks::list_url(&user.name)),
        can_give => crate::user_feedbacks::may_give(page.state(), &page.current, &user),
    };
    let stats = users::profile_stats(db, user.id).await?;
    let history = users::uploads_by_month(db, user.id, CHART_MONTHS).await?;
    let top_tags = users::top_upload_tags(db, user.id, TOP_TAGS).await?;
    let by_user = |path: &str| {
        crate::templates::url_value(&format!(
            "{path}?{}",
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("user", &user.name)
                .finish()
        ))
    };
    let search = |query: String| Value::from_safe_string(search_url(&query));
    // Votes are private, like the searches for them.
    let sees_votes = own || page.current.can(Permission::ApprovePosts);
    let more = context! {
        deleted_uploads => stats.deleted_uploads,
        deleted_uploads_url => search(format!("user:{} status:deleted", user.name)),
        upload_score => stats.upload_score,
        upload_score_url => search(format!("user:{} order:score", user.name)),
        post_changes => stats.post_changes,
        note_changes => stats.note_changes,
        note_changes_url => by_user("/note_versions"),
        wiki_edits => stats.wiki_edits,
        wiki_edits_url => by_user("/wiki_page_versions"),
        pool_edits => stats.pool_edits,
        pool_edits_url => by_user("/pool_versions"),
        forum_posts => stats.forum_posts,
        forum_posts_url => by_user("/forum_posts"),
        approvals => stats.approvals,
        approvals_url => search(format!("approver:{}", user.name)),
        votes => sees_votes.then(|| context! {
            up => stats.upvotes,
            up_url => search(format!("upvote:{}", user.name)),
            down => stats.downvotes,
            down_url => search(format!("downvote:{}", user.name)),
        }),
    };
    let chart = upload_chart(&history);
    let top_tags: Vec<Value> = top_tags
        .into_iter()
        .map(|(name, category, posts)| {
            context! {
                url => search(format!("user:{} {name}", user.name)),
                name => name,
                category => category,
                posts => posts,
            }
        })
        .collect();
    let profile = users::profile(db, user.id).await?;
    let recent_uploads =
        crate::posts::preview(&page, &format!("user:{}", user.name), RECENT_POSTS).await?;
    let recent_favorites =
        crate::posts::preview(&page, &format!("ordfav:{}", user.name), RECENT_POSTS).await?;
    let has_profile =
        !profile.bio.is_empty() || profile.avatar_key.is_some() || profile.banner_key.is_some();
    let comments_url = format!(
        "/comments?{}",
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("user", &user.name)
            .finish()
    );
    Ok(page.render(
        "profile.html",
        context! {
            messages => messages,
            feedback => feedback,
            former_names => former_names,
            invited_by => invited_by,
            more => more,
            chart => chart,
            top_tags => top_tags,
            can_rename => can_rename,
            can_edit_profile => own && page.current.ban.is_none(),
            can_clear_profile => has_profile && can_rename,
            profile => context! {
                bio => crate::profiles::bio_html(&profile),
                has_bio => !profile.bio.is_empty(),
                avatar => crate::profiles::image_url(page.state(), profile.avatar_key.as_deref()),
                banner => crate::profiles::image_url(page.state(), profile.banner_key.as_deref()),
                initial => user.name.chars().next().map(|c| c.to_uppercase().to_string()),
            },
            recent_uploads => recent_uploads,
            recent_favorites => recent_favorites,
            user => context! {
                name => user.name,
                role => role,
                joined => crate::dates::day(user.created_at),
                status => format!("{:?}", user.status).to_lowercase(),
            },
            uploads => uploads,
            uploads_url => Value::from_safe_string(search_url(&format!("user:{}", user.name))),
            favorites => favorites,
            favorites_url => Value::from_safe_string(search_url(&format!("ordfav:{}", user.name))),
            comments => comments,
            promotion => context! {
                promoted => promoted_at.map(crate::dates::day),
                blocked => promotion_blocked,
                can_change => can_manage,
            },
            favorite_groups => favorite_groups,
            favorite_groups_url => crate::templates::url_value(&format!(
                "/favorite_groups?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("user", &user.name)
                    .finish()
            )),
            comments_url => crate::templates::url_value(&comments_url),
            changes_url => crate::templates::url_value(&format!(
                "/post_versions?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("user", &user.name)
                    .finish()
            )),
            bans => ban_history.iter().map(crate::bans::ban_context).collect::<Vec<_>>(),
            can_ban => can_ban,
            banned => banned,
            notes => notes,
            record_url => crate::user_moderation::may_view(&page.current)
                .then(|| crate::templates::url_value(&crate::user_moderation::url(&user.name))),
            can_unban => can_ban && banned,
            durations => crate::bans::durations(),
        },
    ))
}

/// Months the profile's upload chart covers.
const CHART_MONTHS: i32 = 12;
/// Uploads and favorites shown on a profile.
const RECENT_POSTS: u32 = 10;
/// Tags listed as most used in someone's uploads.
const TOP_TAGS: i64 = 10;
/// A bar chart of uploads per month, unless there were none.
fn upload_chart(history: &[(i32, i32, i64)]) -> Option<Value> {
    let bars: Vec<crate::charts::Bar> = history
        .iter()
        .map(|&(year, month, uploads)| {
            let name = u8::try_from(month)
                .ok()
                .and_then(|m| time::Month::try_from(m).ok())
                .map_or_else(String::new, |m| m.to_string());
            crate::charts::Bar {
                label: name.chars().take(3).collect(),
                title: format!("{name} {year}: {uploads}"),
                value: uploads,
            }
        })
        .collect();
    crate::charts::bars(&bars)
}

#[derive(Debug, Deserialize)]
struct AutoPromotionForm {
    /// Present to keep the user from automatic promotion.
    blocked: Option<String>,
}

/// Keeps a user from automatic promotion, or allows it again.
async fn set_auto_promotion(
    page: Page,
    jar: CookieJar,
    Path(name): Path<String>,
    Form(form): Form<AutoPromotionForm>,
) -> Result<Response, AppError> {
    page.current.require(Permission::ManageUsers)?;
    let db = page.state().db.primary();
    let user = users::by_name(db, &name).await?.ok_or(AppError::NotFound)?;
    let outranks = page
        .state()
        .site
        .get()
        .role(user.role_id)
        .is_some_and(|r| page.current.role.outranks(r));
    if !outranks {
        return Err(AppError::Forbidden);
    }
    let blocked = form.blocked.is_some();
    moekura_db::promotion::set_blocked(db, user.id, blocked).await?;
    moekura_db::mod_actions::record(
        db,
        moekura_db::mod_actions::NewAction::new(
            page.current.user.as_ref().map(|u| u.id),
            moekura_core::moderation::ActionKind::UserStatus,
        )
        .user(user.id)
        .details(serde_json::json!({ "auto_promotion_blocked": blocked })),
    )
    .await?;
    let back = format!(
        "/users/{}",
        url::form_urlencoded::byte_serialize(user.name.as_bytes()).collect::<String>()
    );
    Ok((flash::set(jar, Flash::Saved), Redirect::to(&back)).into_response())
}

async fn settings_form(page: Page) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let settings = UserSettings::from_json(&user.settings);
    let blacklist = crate::blacklist::text_for(page.state(), &page.current);
    let has_feed_token = moekura_db::feeds::has_token(page.state().db.primary(), user.id).await?;
    let has_password = users::has_password(page.state().db.primary(), user.id).await?;
    Ok(render_settings(
        &page,
        &settings,
        &blacklist,
        has_feed_token,
        has_password,
        None,
    ))
}

/// `has_password`: whether the user has one to confirm a new feed token
/// with.
fn render_settings(
    page: &Page,
    settings: &UserSettings,
    blacklist: &str,
    has_feed_token: bool,
    has_password: bool,
    error: Option<String>,
) -> Response {
    let max = page.state().search_config().max_per_page;
    let status = if error.is_some() {
        StatusCode::UNPROCESSABLE_ENTITY
    } else {
        StatusCode::OK
    };
    page.render_with_status(
        status,
        "settings.html",
        context! {
            error => error,
            blacklist => blacklist,
            has_feed_token => has_feed_token,
            has_password => has_password,
            fresh_login_minutes => crate::auth::FRESH_LOGIN.as_secs() / 60,
            can_invite => crate::invites::may_invite(&page.current),
            per_page => settings.per_page,
            default_per_page => page.state().search_config().per_page,
            per_page_choices => PER_PAGE_CHOICES.iter().filter(|&&n| n <= max).collect::<Vec<_>>(),
            current_mode => settings.mode.as_str(),
            current_theme => settings.theme,
            prefs_form => context! {
                safe_mode => settings.safe_mode,
                original_images => settings.original_images,
                show_deleted => settings.show_deleted,
                large_thumbnails => settings.large_thumbnails,
                blur_blacklisted => settings.blur_blacklisted,
                hide_comments => settings.hide_comments,
                email_notifications => settings.email_notifications,
                autocomplete => settings.autocomplete,
                shortcuts => settings.shortcuts,
                time_zone => settings.time_zone,
                custom_css => settings.custom_css,
                language => settings.language,
            },
            time_zones => crate::dates::zone_names(),
            // Only worth choosing between several.
            languages => Some(page.state().locales.languages()).filter(|l| l.len() > 1),
            can_view_deleted => page.current.can(Permission::ViewDeleted),
            large_thumbnail_size => page.state().media.config().thumbnail_sizes.get(1),
            site_theme => crate::themes::label(crate::themes::resolve(
                &page.state().assets,
                None,
                &page.state().site.get().settings.default_theme,
            )),
        },
    )
}

#[derive(Debug, Deserialize)]
struct SettingsForm {
    /// Empty for the site default.
    #[serde(default)]
    per_page: String,
    #[serde(default)]
    mode: String,
    /// Empty for the site default.
    #[serde(default)]
    theme: String,
    #[serde(default)]
    blacklist: String,
    // Checkboxes: present when ticked.
    safe_mode: Option<String>,
    original_images: Option<String>,
    show_deleted: Option<String>,
    large_thumbnails: Option<String>,
    blur_blacklisted: Option<String>,
    hide_comments: Option<String>,
    email_notifications: Option<String>,
    autocomplete: Option<String>,
    shortcuts: Option<String>,
    /// Empty for UTC.
    #[serde(default)]
    time_zone: String,
    #[serde(default)]
    custom_css: String,
    /// Empty to follow the browser.
    #[serde(default)]
    language: String,
}

fn parse_mode(text: &str) -> Result<Mode, AppError> {
    Mode::parse(text).ok_or_else(|| AppError::BadRequest("Unknown mode".into()))
}

/// A theme the site has, or `None` for an empty field: the site default.
fn parse_theme(page: &Page, text: &str) -> Result<Option<String>, AppError> {
    if text.is_empty() {
        return Ok(None);
    }
    crate::themes::names(&page.state().assets)
        .contains(&text)
        .then(|| Some(text.to_owned()))
        .ok_or_else(|| AppError::BadRequest("Unknown theme".into()))
}

async fn save_settings(
    page: Page,
    jar: CookieJar,
    Form(form): Form<SettingsForm>,
) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let max = page.state().search_config().max_per_page;
    let per_page = match form.per_page.as_str() {
        "" => None,
        n => Some(
            n.parse::<u32>()
                .ok()
                .filter(|n| PER_PAGE_CHOICES.contains(n) && *n <= max)
                .ok_or_else(|| AppError::BadRequest("Unknown page size".into()))?,
        ),
    };
    let mode = parse_mode(&form.mode)?;
    let theme = parse_theme(&page, &form.theme)?;
    let blacklist = form.blacklist.replace("\r\n", "\n");
    let time_zone = match form.time_zone.as_str() {
        "" => None,
        name if crate::dates::zone_names().iter().any(|n| n == name) => Some(name.to_owned()),
        _ => return Err(AppError::BadRequest("Unknown time zone".into())),
    };
    let custom_css = form.custom_css.replace("\r\n", "\n").trim().to_owned();
    let language = match form.language.as_str() {
        "" => None,
        tag if page.state().locales.has(tag) => Some(tag.to_owned()),
        _ => return Err(AppError::BadRequest("Unknown language".into())),
    };
    let settings = UserSettings {
        per_page,
        mode,
        theme,
        blacklist: Some(blacklist.trim().to_owned()),
        safe_mode: form.safe_mode.is_some(),
        original_images: form.original_images.is_some(),
        show_deleted: form.show_deleted.is_some() && page.current.can(Permission::ViewDeleted),
        large_thumbnails: form.large_thumbnails.is_some(),
        blur_blacklisted: form.blur_blacklisted.is_some(),
        time_zone,
        hide_comments: form.hide_comments.is_some(),
        email_notifications: form.email_notifications.is_some(),
        autocomplete: form.autocomplete.is_some(),
        shortcuts: form.shortcuts.is_some(),
        custom_css,
        language,
    };
    let error = match Blacklist::parse(&blacklist) {
        Err(error) => Some(error.to_string()),
        Ok(_) if settings.custom_css.len() > MAX_CUSTOM_CSS => Some(format!(
            "Custom CSS can be at most {} KB",
            MAX_CUSTOM_CSS / 1024
        )),
        Ok(_) => None,
    };
    if let Some(error) = error {
        let has_feed_token =
            moekura_db::feeds::has_token(page.state().db.primary(), user.id).await?;
        let has_password = users::has_password(page.state().db.primary(), user.id).await?;
        return Ok(render_settings(
            &page,
            &settings,
            &blacklist,
            has_feed_token,
            has_password,
            Some(error),
        ));
    }
    users::set_settings(
        page.state().db.primary(),
        user.id,
        &settings.to_json(&user.settings),
    )
    .await?;
    Ok((flash::set(jar, Flash::Saved), Redirect::to("/settings")).into_response())
}

/// Where the layout loads `settings`' custom stylesheet from, if there is
/// one. The URL changes with the stylesheet, so it can be cached forever.
pub(crate) fn custom_css_url(settings: &UserSettings) -> Option<String> {
    use sha2::{Digest, Sha256};
    (!settings.custom_css.is_empty()).then(|| {
        let digest = Sha256::digest(settings.custom_css.as_bytes());
        format!("/settings/custom.css?v={}", hex::encode(&digest[..8]))
    })
}

/// The user's custom stylesheet. It lives at its own URL because the CSP
/// forbids inline styles.
async fn custom_css(current: crate::auth::CurrentUser) -> Response {
    let css = current
        .user
        .as_ref()
        .map(|u| UserSettings::from_json(&u.settings).custom_css)
        .unwrap_or_default();
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (
                axum::http::header::CACHE_CONTROL,
                "private, max-age=31536000, immutable",
            ),
        ],
        css,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct ThemeForm {
    mode: Option<String>,
    theme: Option<String>,
    /// The page to go back to.
    back: Option<String>,
}

/// Changes only the mode or theme, from the switcher in every page's
/// footer.
async fn set_theme(page: Page, Form(form): Form<ThemeForm>) -> Result<Response, AppError> {
    let user = page.current.user.as_ref().ok_or(AppError::Unauthorized)?;
    let mut settings = UserSettings::from_json(&user.settings);
    if let Some(mode) = &form.mode {
        settings.mode = parse_mode(mode)?;
    }
    if let Some(theme) = &form.theme {
        settings.theme = parse_theme(&page, theme)?;
    }
    users::set_settings(
        page.state().db.primary(),
        user.id,
        &settings.to_json(&user.settings),
    )
    .await?;
    Ok(Redirect::to(crate::account::safe_next(form.back.as_deref())).into_response())
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use crate::test_support::{TestApp, session_for, test_state};

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn profiles_count_what_users_did(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let janitor = session_for(&pool, "janitor", SystemRole::Janitor).await;
        let tag = |name: &'static str, category: i16| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, i32>(
                    "INSERT INTO tags (name, category_id) VALUES ($1, $2) RETURNING id",
                )
                .bind(name)
                .bind(category)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        let (cat, ears, hires) = (
            tag("cat", 0).await,
            tag("ears", 0).await,
            tag("highres", 5).await,
        );
        for (status, score, tags) in [
            ("active", 3, vec![cat, ears, hires]),
            ("active", 2, vec![cat, hires]),
            ("deleted", 9, vec![ears]),
        ] {
            sqlx::query(
                "INSERT INTO posts (rating, status, score, tag_ids, uploader_id, approver_id)
                 SELECT 'g', $1, $2, $3, id,
                        (SELECT id FROM users WHERE name = 'janitor')
                 FROM users WHERE name = 'alice'",
            )
            .bind(status)
            .bind(score)
            .bind(tags)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO post_votes (user_id, post_id, score)
             SELECT u.id, p.id, 1 FROM users u, posts p WHERE u.name = 'alice'",
        )
        .execute(&pool)
        .await
        .unwrap();

        let profile = app.get("/users/alice", Some(&bob)).await.body;
        assert!(
            profile.contains("user%3Aalice+status%3Adeleted\">1 deleted</a>"),
            "{profile}"
        );
        // Alice's own upvotes count too.
        assert!(profile.contains(
            "<dt>Upload score</dt><dd><a href=\"/posts?tags=user%3Aalice+order%3Ascore\">7</a>"
        ));
        assert!(
            profile.contains(
                "<dt>Wiki edits</dt><dd><a href=\"/wiki_page_versions?user=alice\">0</a>"
            )
        );
        assert!(
            profile.contains("<dt>Forum posts</dt><dd><a href=\"/forum_posts?user=alice\">0</a>")
        );
        // Votes are only for them and staff; approvals only for approvers.
        assert!(!profile.contains("<dt>Votes</dt>") && !profile.contains("<dt>Approvals</dt>"));
        assert!(
            app.get("/users/alice", Some(&alice))
                .await
                .body
                .contains("upvote%3Aalice\">3 up</a>")
        );
        assert!(
            app.get("/users/janitor", Some(&janitor))
                .await
                .body
                .contains("<dt>Approvals</dt><dd><a href=\"/posts?tags=approver%3Ajanitor\">3</a>")
        );

        // This month's uploads are charted, and their tags counted, leaving
        // out deleted posts and meta tags.
        assert!(profile.contains("class=\"bar-chart\""));
        assert!(profile.contains(": 3</title>"), "{profile}");
        let top = &profile[profile.find("top-tags").unwrap()..];
        assert!(
            top.find(">cat</a> <span class=\"hint\">2<").unwrap() < top.find(">ears</a>").unwrap()
        );
        assert!(!top.contains(">highres<"));
        assert!(!app.get("/users/bob", None).await.body.contains("bar-chart"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn profiles_and_settings(pool: PgPool) {
        moekura_db::settings::set(&pool, "default_blacklist", serde_json::json!("rating:e"))
            .await
            .unwrap();
        let app = TestApp::new(
            test_state(&pool).await,
            super::routes().merge(crate::posts::routes()),
        );
        let alice = session_for(&pool, "alice", SystemRole::Member).await;

        let profile = app.get("/users/ALICE", None).await;
        assert_eq!(profile.status, StatusCode::OK);
        assert!(profile.body.contains("Member"), "{}", profile.body);
        assert!(profile.body.contains("href=\"/posts?tags=user%3Aalice\""));
        assert!(profile.body.contains("href=\"/posts?tags=ordfav%3Aalice\""));
        assert!(profile.body.contains("href=\"/comments?user=alice\""));
        assert_eq!(
            app.get("/users/nobody", None).await.status,
            StatusCode::NOT_FOUND
        );

        // Visitors are sent to log in first.
        let visitor = app.get("/settings", None).await;
        assert_eq!(visitor.location.as_deref(), Some("/login?next=%2Fsettings"));
        let response = app
            .post_form(
                "/settings",
                Some(&alice),
                &[],
                "per_page=100&mode=dark&theme=ocean",
            )
            .await;
        assert_eq!(response.status, StatusCode::SEE_OTHER);
        let page = app.get("/settings", Some(&alice)).await;
        assert!(
            page.body
                .contains("data-theme=\"ocean\" data-mode=\"dark\""),
            "{}",
            page.body
        );
        assert!(page.body.contains("/static/themes/ocean."), "{}", page.body);
        assert!(
            page.body.contains("value=\"100\" selected"),
            "{}",
            page.body
        );
        // The page size applies to searches.
        let grid = app.get("/", Some(&alice)).await;
        assert_eq!(grid.status, StatusCode::OK);
        // Only themes the site has.
        let unknown = app
            .post_form("/settings", Some(&alice), &[], "mode=dark&theme=nope")
            .await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);

        // The blacklist starts as the site default.
        let bob = session_for(&pool, "bob", SystemRole::Member).await;
        let page = app.get("/settings", Some(&bob)).await;
        assert!(page.body.contains(">rating:e</textarea>"), "{}", page.body);
        let response = app
            .post_form(
                "/settings",
                Some(&bob),
                &[],
                "mode=system&blacklist=score%3A1",
            )
            .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(
            response.body.contains("only tags, -tags and rating:"),
            "{}",
            response.body
        );

        let bad = app
            .post_form("/settings", Some(&alice), &[], "per_page=7&mode=dark")
            .await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);

        // The footer switcher changes the mode or theme alone and goes back.
        let switched = app
            .post_form(
                "/settings/theme",
                Some(&alice),
                &[],
                "mode=light&back=%2Ftags%3Fname%3Dx",
            )
            .await;
        assert_eq!(switched.status, StatusCode::SEE_OTHER);
        assert_eq!(switched.location.as_deref(), Some("/tags?name=x"));
        let page = app.get("/settings", Some(&alice)).await;
        assert!(
            page.body
                .contains("data-theme=\"ocean\" data-mode=\"light\""),
            "{}",
            page.body
        );
        app.post_form("/settings/theme", Some(&alice), &[], "theme=sakura")
            .await;
        let page = app.get("/settings", Some(&alice)).await;
        assert!(
            page.body
                .contains("data-theme=\"sakura\" data-mode=\"light\""),
            "{}",
            page.body
        );
        assert!(
            page.body.contains("value=\"100\" selected"),
            "{}",
            page.body
        );
        let offsite = app
            .post_form(
                "/settings/theme",
                Some(&alice),
                &[],
                "mode=dark&back=%2F%2Fevil.example",
            )
            .await;
        assert_eq!(offsite.location.as_deref(), Some("/"));
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn display_settings(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        let alice = session_for(&pool, "alice", SystemRole::Member).await;
        // Ticked boxes are on; unticked ones off, the default-on ones too.
        let saved = app
            .post_form(
                "/settings",
                Some(&alice),
                &[],
                "mode=system&safe_mode=1&hide_comments=1&time_zone=Europe%2FBerlin\
                 &custom_css=body+%7B+color%3A+red+%7D",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        let page = app.get("/settings", Some(&alice)).await;
        assert!(
            page.body.contains("name=\"safe_mode\" value=\"1\" checked"),
            "{}",
            page.body
        );
        assert!(!page.body.contains("name=\"shortcuts\" value=\"1\" checked"));
        assert!(
            page.body
                .contains("<option selected>Europe&#x2f;Berlin</option>")
        );
        assert!(page.body.contains("data-autocomplete=\"off\""));
        let start = page.body.find("/settings/custom.css?v=").unwrap();
        let end = start + page.body[start..].find('"').unwrap();
        let css = app.get_full(&page.body[start..end]).await;
        assert_eq!(css.headers()["content-type"], "text/css; charset=utf-8");
        let css = app.get(&page.body[start..end], Some(&alice)).await;
        assert_eq!(css.body, "body { color: red }");

        let unknown = app
            .post_form(
                "/settings",
                Some(&alice),
                &[],
                "mode=system&time_zone=Mars%2FBase",
            )
            .await;
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
        let big = format!("mode=system&custom_css={}", "a".repeat(65 * 1024));
        let refused = app.post_form("/settings", Some(&alice), &[], &big).await;
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(refused.body.contains("at most 64 KB"), "{}", refused.body);
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn staff_keep_users_from_promotion(pool: PgPool) {
        let app = TestApp::new(test_state(&pool).await, super::routes());
        session_for(&pool, "alice", SystemRole::Member).await;
        let member = session_for(&pool, "bob", SystemRole::Member).await;
        let admin = session_for(&pool, "root", SystemRole::Admin).await;
        assert!(
            !app.get("/users/alice", Some(&member))
                .await
                .body
                .contains("auto-promotion")
        );
        let page = app.get("/users/alice", Some(&admin)).await;
        assert!(
            page.body.contains("Never promote automatically"),
            "{}",
            page.body
        );
        assert_eq!(
            app.post_form(
                "/users/alice/auto-promotion",
                Some(&member),
                &[],
                "blocked=1"
            )
            .await
            .status,
            StatusCode::FORBIDDEN
        );
        let saved = app
            .post_form(
                "/users/alice/auto-promotion",
                Some(&admin),
                &[],
                "blocked=1",
            )
            .await;
        assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
        assert!(
            app.get("/users/alice", Some(&admin))
                .await
                .body
                .contains("Kept from automatic promotion.")
        );
        // Not someone of the same rank.
        session_for(&pool, "root2", SystemRole::Admin).await;
        assert_eq!(
            app.post_form("/users/root2/auto-promotion", Some(&admin), &[], "")
                .await
                .status,
            StatusCode::FORBIDDEN
        );
    }
}
