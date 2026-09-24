//! Creating accounts and checking credentials.
//!
//! Password hashing is CPU-heavy, so it runs on tokio's blocking pool
//! rather than stalling the async workers.

use moekura_core::accounts::{self, EmailError, NameError, PasswordError, UserName, Verification};
use sqlx::{PgExecutor, PgPool};

use crate::users::{self, InsertError, NewUser, User, UserStatus};

pub struct NewAccount<'a> {
    pub name: &'a str,
    pub password: &'a str,
    pub email: Option<&'a str>,
    pub role_id: i32,
    pub status: UserStatus,
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("name {0}")]
    InvalidName(NameError),
    #[error("password {0}")]
    InvalidPassword(PasswordError),
    #[error("email {0}")]
    InvalidEmail(EmailError),
    #[error("name is already taken")]
    NameTaken,
    #[error("email is already in use")]
    EmailTaken,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Validates and hashes before touching `db`, which may be a transaction
/// (e.g. one that also redeems an invite).
pub async fn create(db: impl PgExecutor<'_>, account: NewAccount<'_>) -> Result<User, CreateError> {
    let name = UserName::parse(account.name).map_err(CreateError::InvalidName)?;
    accounts::check_password(account.password).map_err(CreateError::InvalidPassword)?;
    let email = account.email.map(str::trim).filter(|e| !e.is_empty());
    if let Some(email) = email {
        accounts::check_email(email).map_err(CreateError::InvalidEmail)?;
    }

    let password = account.password.to_owned();
    let hash = blocking(move || accounts::hash_password(&password)).await;

    let new_user = NewUser {
        name: name.as_str(),
        email,
        password_hash: Some(&hash),
        role_id: account.role_id,
        status: account.status,
    };
    users::insert(db, new_user).await.map_err(|e| match e {
        InsertError::NameTaken => CreateError::NameTaken,
        InsertError::EmailTaken => CreateError::EmailTaken,
        InsertError::Db(e) => CreateError::Db(e),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Unknown name or wrong password; deliberately indistinguishable.
    #[error("invalid name or password")]
    InvalidCredentials,
    #[error("account is awaiting approval")]
    Pending,
    #[error("account's email address is not confirmed yet")]
    Unverified,
    #[error("account is deactivated")]
    Deactivated,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Checks a name and password. Takes about as long whether or not the
/// account exists, so response times don't reveal which names are taken.
pub async fn authenticate(db: &PgPool, name: &str, password: &str) -> Result<User, AuthError> {
    let found = users::credentials_by_name(db, name).await?;
    let password = password.to_owned();

    let Some((user, Some(hash))) = found else {
        blocking(move || accounts::verify_dummy_password(&password)).await;
        return Err(AuthError::InvalidCredentials);
    };

    let checked = password.clone();
    let needs_rehash = match blocking(move || accounts::verify_password(&checked, &hash)).await {
        Verification::Invalid => return Err(AuthError::InvalidCredentials),
        Verification::Valid { needs_rehash } => needs_rehash,
    };

    match user.status {
        UserStatus::Active => {}
        UserStatus::Pending => return Err(AuthError::Pending),
        UserStatus::Unverified => return Err(AuthError::Unverified),
        UserStatus::Deactivated => return Err(AuthError::Deactivated),
    }

    if needs_rehash {
        let new_hash = blocking(move || accounts::hash_password(&password)).await;
        // Best effort: the login is valid either way.
        if let Err(error) = users::set_password_hash(db, user.id, &new_hash).await {
            tracing::warn!(%error, user_id = user.id, "could not upgrade password hash");
        }
    }
    Ok(user)
}

/// Whether `password` is user `id`'s, for confirming a change to their
/// account. False for accounts without a password.
pub async fn check_password_of(db: &PgPool, id: i64, password: &str) -> sqlx::Result<bool> {
    let hash: Option<Option<String>> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(db)
            .await?;
    let password = password.to_owned();
    let Some(Some(hash)) = hash else {
        blocking(move || accounts::verify_dummy_password(&password)).await;
        return Ok(false);
    };
    let verification = blocking(move || accounts::verify_password(&password, &hash)).await;
    Ok(matches!(verification, Verification::Valid { .. }))
}

#[derive(Debug, thiserror::Error)]
pub enum PasswordChangeError {
    #[error("password {0}")]
    Invalid(PasswordError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Checks and sets a new password for user `id`. Ending their sessions is
/// up to the caller.
pub async fn set_password(
    db: impl PgExecutor<'_>,
    id: i64,
    password: &str,
) -> Result<(), PasswordChangeError> {
    accounts::check_password(password).map_err(PasswordChangeError::Invalid)?;
    let password = password.to_owned();
    let hash = blocking(move || accounts::hash_password(&password)).await;
    users::set_password_hash(db, id, &hash).await?;
    Ok(())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("password hashing panicked")
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;

    use super::*;
    use crate::roles;

    async fn member(db: &PgPool) -> i32 {
        roles::by_system(db, SystemRole::Member).await.unwrap().id
    }

    async fn create_active(db: &PgPool, name: &str, password: &str) -> Result<User, CreateError> {
        let role_id = member(db).await;
        let account = NewAccount {
            name,
            password,
            email: None,
            role_id,
            status: UserStatus::Active,
        };
        create(db, account).await
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn create_then_authenticate(pool: PgPool) {
        let user = create_active(&pool, "catherine", "correct horse")
            .await
            .unwrap();
        assert_eq!(user.status, UserStatus::Active);
        let authed = authenticate(&pool, "CATHERINE", "correct horse")
            .await
            .unwrap();
        assert_eq!(authed.id, user.id);
        assert_eq!(authed.name, "catherine");
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn names_are_unique_case_insensitively(pool: PgPool) {
        create_active(&pool, "catherine", "correct horse")
            .await
            .unwrap();
        let err = create_active(&pool, "Catherine", "correct horse")
            .await
            .unwrap_err();
        assert!(matches!(err, CreateError::NameTaken), "{err:?}");
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn emails_are_unique_and_optional(pool: PgPool) {
        let role_id = member(&pool).await;
        let with_email = |name, email| NewAccount {
            name,
            password: "correct horse",
            email,
            role_id,
            status: UserStatus::Active,
        };
        create(&pool, with_email("one", Some("a@example.com")))
            .await
            .unwrap();
        // Blank counts as no email, so it doesn't collide.
        create(&pool, with_email("two", Some("  "))).await.unwrap();
        create(&pool, with_email("three", None)).await.unwrap();
        let err = create(&pool, with_email("four", Some("A@example.com")))
            .await
            .unwrap_err();
        assert!(matches!(err, CreateError::EmailTaken), "{err:?}");
        let err = create(&pool, with_email("five", Some("nope")))
            .await
            .unwrap_err();
        assert!(matches!(err, CreateError::InvalidEmail(_)), "{err:?}");
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn validates_before_touching_the_database(pool: PgPool) {
        let err = create_active(&pool, "has space", "correct horse")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            CreateError::InvalidName(NameError::InvalidCharacters)
        ));
        let err = create_active(&pool, "fine", "short").await.unwrap_err();
        assert!(matches!(
            err,
            CreateError::InvalidPassword(PasswordError::TooShort)
        ));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn rejects_wrong_password_and_unknown_user_alike(pool: PgPool) {
        create_active(&pool, "catherine", "correct horse")
            .await
            .unwrap();
        let wrong = authenticate(&pool, "catherine", "wrong horse")
            .await
            .unwrap_err();
        let unknown = authenticate(&pool, "nobody", "correct horse")
            .await
            .unwrap_err();
        assert!(matches!(wrong, AuthError::InvalidCredentials));
        assert!(matches!(unknown, AuthError::InvalidCredentials));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn inactive_accounts_cannot_log_in(pool: PgPool) {
        let user = create_active(&pool, "catherine", "correct horse")
            .await
            .unwrap();
        users::set_status(&pool, user.id, UserStatus::Pending)
            .await
            .unwrap();
        let err = authenticate(&pool, "catherine", "correct horse")
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::Pending));
        users::set_status(&pool, user.id, UserStatus::Deactivated)
            .await
            .unwrap();
        let err = authenticate(&pool, "catherine", "correct horse")
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::Deactivated));
        // A wrong password on an inactive account reveals nothing about it.
        let err = authenticate(&pool, "catherine", "wrong horse")
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::InvalidCredentials));
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn upgrades_outdated_hashes_on_login(pool: PgPool) {
        use argon2::password_hash::PasswordHasher;

        let user = create_active(&pool, "catherine", "correct horse")
            .await
            .unwrap();
        let weak = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(8 * 1024, 1, 1, None).unwrap(),
        )
        .hash_password(b"correct horse")
        .unwrap()
        .to_string();
        users::set_password_hash(&pool, user.id, &weak)
            .await
            .unwrap();

        authenticate(&pool, "catherine", "correct horse")
            .await
            .unwrap();
        let (_, stored) = users::credentials_by_name(&pool, "catherine")
            .await
            .unwrap()
            .unwrap();
        let stored = stored.unwrap();
        assert_ne!(stored, weak);
        assert!(stored.contains("m=19456"), "{stored}");
    }
}
