//! Queries on `users`.

use sqlx::PgExecutor;
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    /// Registered while registration required approval; cannot log in yet.
    Pending,
    /// Registered while the site checked email addresses, and hasn't
    /// followed the link sent to theirs; cannot log in yet.
    Unverified,
    Deactivated,
}

impl UserStatus {
    pub const ALL: [UserStatus; 4] = [
        UserStatus::Active,
        UserStatus::Pending,
        UserStatus::Unverified,
        UserStatus::Deactivated,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            UserStatus::Active => "active",
            UserStatus::Pending => "pending",
            UserStatus::Unverified => "unverified",
            UserStatus::Deactivated => "deactivated",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    /// When `email` was confirmed through a link sent to it.
    pub email_verified_at: Option<OffsetDateTime>,
    pub role_id: i32,
    pub status: UserStatus,
    pub created_at: OffsetDateTime,
    pub last_seen_at: Option<OffsetDateTime>,
    /// Preferences as stored; read with `moekura_core::user_settings`.
    pub settings: serde_json::Value,
}

pub struct NewUser<'a> {
    pub name: &'a str,
    pub email: Option<&'a str>,
    pub password_hash: Option<&'a str>,
    pub role_id: i32,
    pub status: UserStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum InsertError {
    #[error("name is already taken")]
    NameTaken,
    #[error("email is already in use")]
    EmailTaken,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// `SELECT <user columns> FROM users` followed by `$rest`.
macro_rules! select_users {
    ($rest:literal) => {
        concat!(
            "SELECT id, name::text, email::text, email_verified_at, role_id, status, created_at, last_seen_at, settings FROM users ",
            $rest
        )
    };
}

pub async fn insert(db: impl PgExecutor<'_>, user: NewUser<'_>) -> Result<User, InsertError> {
    sqlx::query_as(
        "INSERT INTO users (name, email, password_hash, role_id, status)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, name::text, email::text, email_verified_at, role_id, status, created_at, last_seen_at, settings",
    )
    .bind(user.name)
    .bind(user.email)
    .bind(user.password_hash)
    .bind(user.role_id)
    .bind(user.status)
    .fetch_one(db)
    .await
    .map_err(|error| match unique_violation(&error) {
        Some("users_name_key") => InsertError::NameTaken,
        Some("users_email_key") => InsertError::EmailTaken,
        _ => InsertError::Db(error),
    })
}

pub async fn by_id(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Option<User>> {
    sqlx::query_as(select_users!("WHERE id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
}

/// The names of the users among `ids`, as (id, name).
pub async fn names(db: impl PgExecutor<'_>, ids: &[i64]) -> sqlx::Result<Vec<(i64, String)>> {
    sqlx::query_as("SELECT id, name::text FROM users WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(db)
        .await
}

/// The users with the given ids, in no particular order.
pub async fn by_ids(db: impl PgExecutor<'_>, ids: &[i64]) -> sqlx::Result<Vec<User>> {
    sqlx::query_as(select_users!("WHERE id = ANY($1)"))
        .bind(ids)
        .fetch_all(db)
        .await
}

/// What a user has done, for profiles.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Activity {
    pub uploads: i64,
    /// Post changes after the upload.
    pub edits: i64,
    pub favorites: i64,
    /// A ban is in force.
    pub banned: bool,
}

pub async fn activity(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Activity> {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM posts WHERE uploader_id = $1) AS uploads,
                (SELECT count(*) FROM post_versions WHERE updater_id = $1 AND version > 1) AS edits,
                (SELECT count(*) FROM favorites WHERE user_id = $1) AS favorites,
                EXISTS (SELECT 1 FROM bans WHERE user_id = $1 AND lifted_at IS NULL
                        AND (expires_at IS NULL OR expires_at > now())) AS banned",
    )
    .bind(id)
    .fetch_one(db)
    .await
}

/// More of what a user has done, for their profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct ProfileStats {
    /// Their uploads since deleted.
    pub deleted_uploads: i64,
    /// The scores of their uploads that remain, added up.
    pub upload_score: i64,
    pub post_changes: i64,
    pub note_changes: i64,
    pub wiki_edits: i64,
    pub pool_edits: i64,
    /// Not counting hidden ones.
    pub forum_posts: i64,
    /// Posts they approved.
    pub approvals: i64,
    pub upvotes: i64,
    pub downvotes: i64,
}

pub async fn profile_stats(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<ProfileStats> {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM posts WHERE uploader_id = $1 AND status = 'deleted')
                    AS deleted_uploads,
                (SELECT coalesce(sum(score), 0)::bigint FROM posts
                 WHERE uploader_id = $1 AND status <> 'deleted') AS upload_score,
                (SELECT count(*) FROM post_versions WHERE updater_id = $1) AS post_changes,
                (SELECT count(*) FROM note_versions WHERE updater_id = $1) AS note_changes,
                (SELECT count(*) FROM wiki_page_versions WHERE updater_id = $1) AS wiki_edits,
                (SELECT count(*) FROM pool_versions WHERE updater_id = $1) AS pool_edits,
                (SELECT count(*) FROM forum_posts WHERE creator_id = $1 AND NOT is_hidden)
                    AS forum_posts,
                (SELECT count(*) FROM posts WHERE approver_id = $1) AS approvals,
                (SELECT count(*) FROM post_votes WHERE user_id = $1 AND score > 0) AS upvotes,
                (SELECT count(*) FROM post_votes WHERE user_id = $1 AND score < 0) AS downvotes",
    )
    .bind(id)
    .fetch_one(db)
    .await
}

/// A user's uploads in each of the last `months` months (UTC), oldest
/// first, as `(year, month, uploads)`.
pub async fn uploads_by_month(
    db: impl PgExecutor<'_>,
    id: i64,
    months: i32,
) -> sqlx::Result<Vec<(i32, i32, i64)>> {
    sqlx::query_as(
        "SELECT extract(year FROM m)::int, extract(month FROM m)::int, count(p.id)
         FROM generate_series(
                  date_trunc('month', now() AT TIME ZONE 'UTC') - make_interval(months => $2 - 1),
                  date_trunc('month', now() AT TIME ZONE 'UTC'),
                  interval '1 month') AS m
         LEFT JOIN posts p ON p.uploader_id = $1
              AND p.created_at >= m AT TIME ZONE 'UTC'
              AND p.created_at < (m + interval '1 month') AT TIME ZONE 'UTC'
         GROUP BY m ORDER BY m",
    )
    .bind(id)
    .bind(months)
    .fetch_all(db)
    .await
}

/// How many recent uploads [`top_upload_tags`] looks at.
pub const TOP_TAGS_UPLOADS: i64 = 1000;

/// The tags most used on a user's latest [`TOP_TAGS_UPLOADS`] remaining
/// uploads, leaving out meta tags, as `(name, category name, posts)`.
pub async fn top_upload_tags(
    db: impl PgExecutor<'_>,
    id: i64,
    limit: i64,
) -> sqlx::Result<Vec<(String, String, i64)>> {
    sqlx::query_as(
        "SELECT t.name::text, c.name, count(*)
         FROM (SELECT tag_ids FROM posts WHERE uploader_id = $1 AND status <> 'deleted'
               ORDER BY id DESC LIMIT $3) AS p
         CROSS JOIN LATERAL unnest(p.tag_ids) AS tag_id
         JOIN tags t ON t.id = tag_id
         JOIN tag_categories c ON c.id = t.category_id
         WHERE c.name <> 'meta'
         GROUP BY t.id, t.name, c.name
         ORDER BY count(*) DESC, t.name LIMIT $2",
    )
    .bind(id)
    .bind(limit)
    .bind(TOP_TAGS_UPLOADS)
    .fetch_all(db)
    .await
}

/// Case-insensitive.
pub async fn by_name(db: impl PgExecutor<'_>, name: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as(select_users!("WHERE name = $1::citext"))
        .bind(name)
        .fetch_optional(db)
        .await
}

/// The user called `name` now or, failing that, the one who was called
/// that most recently.
pub async fn by_name_or_former(db: &sqlx::PgPool, name: &str) -> sqlx::Result<Option<User>> {
    if let Some(user) = by_name(db, name).await? {
        return Ok(Some(user));
    }
    sqlx::query_as(select_users!(
        "WHERE id = (SELECT user_id FROM user_name_changes WHERE old_name = $1::citext
                     ORDER BY id DESC LIMIT 1)"
    ))
    .bind(name)
    .fetch_optional(db)
    .await
}

/// The user and their password hash, for login.
pub async fn credentials_by_name(
    db: impl PgExecutor<'_>,
    name: &str,
) -> sqlx::Result<Option<(User, Option<String>)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        user: User,
        password_hash: Option<String>,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT id, name::text, email::text, email_verified_at, role_id, status, created_at, last_seen_at, settings, password_hash
         FROM users WHERE name = $1::citext",
    )
    .bind(name)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| (r.user, r.password_hash)))
}

pub async fn set_password_hash(db: impl PgExecutor<'_>, id: i64, hash: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
        .bind(id)
        .bind(hash)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_settings(
    db: impl PgExecutor<'_>,
    id: i64,
    settings: &serde_json::Value,
) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET settings = $2 WHERE id = $1")
        .bind(id)
        .bind(settings)
        .execute(db)
        .await?;
    Ok(())
}

/// What a user wrote and chose for their profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Profile {
    /// In markup; empty for none.
    pub bio: String,
    /// Storage keys of the profile picture and banner.
    pub avatar_key: Option<String>,
    pub banner_key: Option<String>,
}

pub async fn profile(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<Profile> {
    sqlx::query_as("SELECT bio, avatar_key, banner_key FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
        .map(Option::unwrap_or_default)
}

pub async fn set_profile(db: impl PgExecutor<'_>, id: i64, profile: &Profile) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET bio = $2, avatar_key = $3, banner_key = $4 WHERE id = $1")
        .bind(id)
        .bind(&profile.bio)
        .bind(&profile.avatar_key)
        .bind(&profile.banner_key)
        .execute(db)
        .await?;
    Ok(())
}

/// Whether anyone's profile picture or banner is the file at `key`. Keys
/// are content-addressed, so two users can share one.
pub async fn profile_image_used(db: impl PgExecutor<'_>, key: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM users WHERE avatar_key = $1 OR banner_key = $1)",
    )
    .bind(key)
    .fetch_one(db)
    .await
}

/// Which users [`list`] finds. Empty text and `None` match anyone.
#[derive(Debug, Clone, Copy, Default)]
pub struct UserFilter<'a> {
    /// The start of the name, case-insensitively.
    pub name_prefix: &'a str,
    pub status: Option<UserStatus>,
    pub role_id: Option<i32>,
    /// Part of the email address, case-insensitively.
    pub email: &'a str,
    /// Whether they're under a ban.
    pub banned: Option<bool>,
}

/// `text` for `LIKE`, with its wildcards escaped.
fn like_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Users matching `filter`, by name.
pub async fn list(
    db: impl PgExecutor<'_>,
    filter: &UserFilter<'_>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<User>> {
    sqlx::query_as(select_users!(
        "WHERE name ILIKE $1 || '%' AND ($2::text IS NULL OR status = $2)
           AND ($3::integer IS NULL OR role_id = $3)
           AND ($4 = '' OR email ILIKE '%' || $4 || '%')
           AND ($5::boolean IS NULL OR $5 = EXISTS (
               SELECT 1 FROM bans b WHERE b.user_id = users.id AND b.lifted_at IS NULL
                 AND (b.expires_at IS NULL OR b.expires_at > now())))
         ORDER BY name OFFSET $6 LIMIT $7"
    ))
    .bind(like_escape(filter.name_prefix))
    .bind(filter.status)
    .bind(filter.role_id)
    .bind(like_escape(filter.email))
    .bind(filter.banned)
    .bind(offset)
    .bind(limit)
    .fetch_all(db)
    .await
}

pub async fn set_role(db: impl PgExecutor<'_>, id: i64, role_id: i32) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET role_id = $2 WHERE id = $1")
        .bind(id)
        .bind(role_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn set_status(db: impl PgExecutor<'_>, id: i64, status: UserStatus) -> sqlx::Result<()> {
    sqlx::query("UPDATE users SET status = $2 WHERE id = $1")
        .bind(id)
        .bind(status)
        .execute(db)
        .await?;
    Ok(())
}

/// Whether the user can log in with a password (accounts made through
/// single sign-on have none).
pub async fn has_password(db: impl PgExecutor<'_>, id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT password_hash IS NOT NULL FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
}

/// Case-insensitive.
pub async fn by_email(db: impl PgExecutor<'_>, email: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as(select_users!("WHERE email = $1::citext"))
        .bind(email)
        .fetch_optional(db)
        .await
}

/// Changes a user's address (`None` removes it). `verified` records that
/// it was confirmed through a link sent to it.
pub async fn set_email(
    db: impl PgExecutor<'_>,
    id: i64,
    email: Option<&str>,
    verified: bool,
) -> Result<(), InsertError> {
    sqlx::query(
        "UPDATE users SET email = $2, email_verified_at = CASE WHEN $3 THEN now() END
         WHERE id = $1",
    )
    .bind(id)
    .bind(email)
    .bind(verified)
    .execute(db)
    .await
    .map_err(|error| match unique_violation(&error) {
        Some("users_email_key") => InsertError::EmailTaken,
        _ => InsertError::Db(error),
    })?;
    Ok(())
}

/// The constraint name when `error` is a unique violation.
pub(crate) fn unique_violation(error: &sqlx::Error) -> Option<&str> {
    match error {
        sqlx::Error::Database(e) if e.is_unique_violation() => e.constraint(),
        _ => None,
    }
}
