//! Queries on `users`.

use sqlx::PgExecutor;
use time::OffsetDateTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    /// Registered while registration required approval; cannot log in yet.
    Pending,
    Deactivated,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub role_id: i32,
    pub status: UserStatus,
    pub created_at: OffsetDateTime,
    pub last_seen_at: Option<OffsetDateTime>,
    /// Preferences as stored; read with `uwuu_core::user_settings`.
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
            "SELECT id, name::text, email::text, role_id, status, created_at, last_seen_at, settings FROM users ",
            $rest
        )
    };
}

pub async fn insert(db: impl PgExecutor<'_>, user: NewUser<'_>) -> Result<User, InsertError> {
    sqlx::query_as(
        "INSERT INTO users (name, email, password_hash, role_id, status)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, name::text, email::text, role_id, status, created_at, last_seen_at, settings",
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
        "SELECT id, name::text, email::text, role_id, status, created_at, last_seen_at, settings, password_hash
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

/// The constraint name when `error` is a unique violation.
pub(crate) fn unique_violation(error: &sqlx::Error) -> Option<&str> {
    match error {
        sqlx::Error::Database(e) if e.is_unique_violation() => e.constraint(),
        _ => None,
    }
}
