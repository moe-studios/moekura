//! Passkeys (WebAuthn credentials) and the registrations and logins
//! waiting for the browser's answer (`user_passkeys`,
//! `passkey_challenges`). The credentials and challenge states are kept
//! as the web crate's WebAuthn library serialises them.

use std::time::Duration;

use moekura_core::tokens::{NewToken, hash_token};
use serde_json::Value;
use sqlx::PgExecutor;
use time::OffsetDateTime;

/// Most passkeys one account can have.
pub const MAX_PER_USER: i64 = 20;

/// Longest name for a passkey, in characters.
pub const NAME_MAX_LEN: usize = 64;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Passkey {
    pub id: i64,
    pub user_id: i64,
    pub credential_id: Vec<u8>,
    pub user_handle: Vec<u8>,
    pub name: String,
    pub credential: Value,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
}

macro_rules! select_passkeys {
    ($rest:literal) => {
        concat!(
            "SELECT id, user_id, credential_id, user_handle, name, credential, created_at,
             last_used_at FROM user_passkeys ",
            $rest
        )
    };
}

/// The user's passkeys, oldest first.
pub async fn for_user(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<Vec<Passkey>> {
    sqlx::query_as(select_passkeys!("WHERE user_id = $1 ORDER BY id"))
        .bind(user_id)
        .fetch_all(db)
        .await
}

/// Whether the user has a passkey.
pub async fn has_any(db: impl PgExecutor<'_>, user_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM user_passkeys WHERE user_id = $1)")
        .bind(user_id)
        .fetch_one(db)
        .await
}

/// The passkey the authenticator calls `credential_id`, whoever's it is.
pub async fn by_credential_id(
    db: impl PgExecutor<'_>,
    credential_id: &[u8],
) -> sqlx::Result<Option<Passkey>> {
    sqlx::query_as(select_passkeys!("WHERE credential_id = $1"))
        .bind(credential_id)
        .fetch_optional(db)
        .await
}

pub struct NewPasskey<'a> {
    pub user_id: i64,
    pub credential_id: &'a [u8],
    pub user_handle: &'a [u8],
    pub name: &'a str,
    pub credential: &'a Value,
}

#[derive(Debug, thiserror::Error)]
pub enum AddError {
    #[error("this passkey is already registered")]
    Taken,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Registers a passkey; returns its id.
pub async fn add(db: impl PgExecutor<'_>, new: NewPasskey<'_>) -> Result<i64, AddError> {
    sqlx::query_scalar(
        "INSERT INTO user_passkeys (user_id, credential_id, user_handle, name, credential)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(new.user_id)
    .bind(new.credential_id)
    .bind(new.user_handle)
    .bind(new.name)
    .bind(new.credential)
    .fetch_one(db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => AddError::Taken,
        _ => AddError::Db(e),
    })
}

/// Records a login with passkey `id`, with its `credential` as it is
/// after it (a higher sign count, backup flags).
pub async fn used(db: impl PgExecutor<'_>, id: i64, credential: &Value) -> sqlx::Result<()> {
    sqlx::query("UPDATE user_passkeys SET credential = $2, last_used_at = now() WHERE id = $1")
        .bind(id)
        .bind(credential)
        .execute(db)
        .await?;
    Ok(())
}

/// Renames the user's passkey `id`; false if they have no such passkey.
pub async fn rename(
    db: impl PgExecutor<'_>,
    user_id: i64,
    id: i64,
    name: &str,
) -> sqlx::Result<bool> {
    let updated = sqlx::query("UPDATE user_passkeys SET name = $3 WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .bind(name)
        .execute(db)
        .await?;
    Ok(updated.rows_affected() == 1)
}

/// Removes the user's passkey `id`; false if they have no such passkey.
pub async fn remove(db: impl PgExecutor<'_>, user_id: i64, id: i64) -> sqlx::Result<bool> {
    let deleted = sqlx::query("DELETE FROM user_passkeys WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user_id)
        .execute(db)
        .await?;
    Ok(deleted.rows_affected() == 1)
}

/// What a challenge was made for. Each is only taken back for the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Adding a passkey to a logged-in account.
    Register,
    /// Logging in with a passkey instead of a password.
    Login,
    /// A passkey instead of a code, after the password.
    SecondFactor,
}

impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Purpose::Register => "register",
            Purpose::Login => "login",
            Purpose::SecondFactor => "second_factor",
        }
    }
}

/// Keeps `state` until the browser answers, for `ttl`; returns the token
/// to send it with.
pub async fn challenge(
    db: impl PgExecutor<'_>,
    user_id: Option<i64>,
    purpose: Purpose,
    state: &Value,
    ttl: Duration,
) -> sqlx::Result<String> {
    let token = NewToken::generate();
    // Expired challenges are cleared by prune_challenges.
    sqlx::query(
        "INSERT INTO passkey_challenges (token_hash, user_id, purpose, state, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))",
    )
    .bind(&token.hash[..])
    .bind(user_id)
    .bind(purpose.as_str())
    .bind(state)
    .bind(ttl.as_secs_f64())
    .execute(db)
    .await?;
    Ok(token.token)
}

/// The state behind `token`, made for `purpose` and `user_id`, if it
/// hasn't expired. Taken once: the token stops working either way, so an
/// answer can't be tried twice.
pub async fn take_challenge(
    db: impl PgExecutor<'_>,
    token: &str,
    purpose: Purpose,
    user_id: Option<i64>,
) -> sqlx::Result<Option<Value>> {
    let taken: Option<(Value, Option<i64>, String, bool)> = sqlx::query_as(
        "DELETE FROM passkey_challenges WHERE token_hash = $1
         RETURNING state, user_id, purpose, expires_at > now()",
    )
    .bind(&hash_token(token)[..])
    .fetch_optional(db)
    .await?;
    Ok(taken.and_then(|(state, owner, made_for, live)| {
        (live && owner == user_id && made_for == purpose.as_str()).then_some(state)
    }))
}

/// Removes expired challenges. Returns how many were removed.
pub async fn prune_challenges(db: impl PgExecutor<'_>) -> sqlx::Result<u64> {
    let result = sqlx::query("DELETE FROM passkey_challenges WHERE expires_at < now()")
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;

    async fn user(pool: &PgPool, name: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO users (name, role_id) SELECT $1, id FROM roles WHERE system_key = 'member' RETURNING id",
        )
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    fn new<'a>(user_id: i64, credential_id: &'a [u8], credential: &'a Value) -> NewPasskey<'a> {
        NewPasskey {
            user_id,
            credential_id,
            user_handle: b"handle",
            name: "Phone",
            credential,
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn adding_using_renaming_and_removing(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let bob = user(&pool, "bob").await;
        assert!(!has_any(&pool, alice).await.unwrap());
        let credential = json!({ "counter": 0 });
        let id = add(&pool, new(alice, b"one", &credential)).await.unwrap();
        assert!(has_any(&pool, alice).await.unwrap());
        // A passkey belongs to one account.
        assert!(matches!(
            add(&pool, new(bob, b"one", &credential)).await,
            Err(AddError::Taken)
        ));

        let found = by_credential_id(&pool, b"one").await.unwrap().unwrap();
        assert_eq!((found.id, found.user_id), (id, alice));
        assert_eq!(found.last_used_at, None);
        used(&pool, id, &json!({ "counter": 3 })).await.unwrap();
        let found = by_credential_id(&pool, b"one").await.unwrap().unwrap();
        assert_eq!(found.credential["counter"], 3);
        assert!(found.last_used_at.is_some());

        assert!(!rename(&pool, bob, id, "Mine now").await.unwrap());
        assert!(rename(&pool, alice, id, "Laptop").await.unwrap());
        assert_eq!(for_user(&pool, alice).await.unwrap()[0].name, "Laptop");

        assert!(!remove(&pool, bob, id).await.unwrap());
        assert!(remove(&pool, alice, id).await.unwrap());
        assert!(by_credential_id(&pool, b"one").await.unwrap().is_none());
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn challenges_are_taken_once_for_their_purpose(pool: PgPool) {
        let alice = user(&pool, "alice").await;
        let ttl = Duration::from_secs(300);
        let state = json!({ "challenge": "abc" });

        let token = challenge(&pool, Some(alice), Purpose::Register, &state, ttl)
            .await
            .unwrap();
        let taken = take_challenge(&pool, &token, Purpose::Register, Some(alice)).await;
        assert_eq!(taken.unwrap(), Some(state.clone()));
        let again = take_challenge(&pool, &token, Purpose::Register, Some(alice)).await;
        assert_eq!(again.unwrap(), None);

        // Asking for the wrong purpose or user uses it up too.
        let token = challenge(&pool, None, Purpose::Login, &state, ttl)
            .await
            .unwrap();
        let wrong = take_challenge(&pool, &token, Purpose::SecondFactor, None).await;
        assert_eq!(wrong.unwrap(), None);
        let after = take_challenge(&pool, &token, Purpose::Login, None).await;
        assert_eq!(after.unwrap(), None);
        let token = challenge(&pool, Some(alice), Purpose::SecondFactor, &state, ttl)
            .await
            .unwrap();
        let anyone = take_challenge(&pool, &token, Purpose::SecondFactor, None).await;
        assert_eq!(anyone.unwrap(), None);

        let expired = challenge(&pool, None, Purpose::Login, &state, Duration::ZERO)
            .await
            .unwrap();
        let late = take_challenge(&pool, &expired, Purpose::Login, None).await;
        assert_eq!(late.unwrap(), None);
        challenge(&pool, None, Purpose::Login, &state, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(prune_challenges(&pool).await.unwrap(), 1);
    }
}
