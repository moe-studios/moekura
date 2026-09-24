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

/// Case-insensitive.
pub async fn by_name(db: impl PgExecutor<'_>, name: &str) -> sqlx::Result<Option<User>> {
    sqlx::query_as(select_users!("WHERE name = $1::citext"))
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

/// Users whose names start with `prefix`, optionally only with `status`,
/// by name.
pub async fn list(
    db: impl PgExecutor<'_>,
    prefix: &str,
    status: Option<UserStatus>,
    offset: i64,
    limit: i64,
) -> sqlx::Result<Vec<User>> {
    sqlx::query_as(select_users!(
        "WHERE name ILIKE $1 || '%' AND ($2::text IS NULL OR status = $2)
         ORDER BY name OFFSET $3 LIMIT $4"
    ))
    .bind(
        prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_"),
    )
    .bind(status)
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
