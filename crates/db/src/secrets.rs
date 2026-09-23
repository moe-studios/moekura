//! Keys the application generates once and shares between nodes.

use sqlx::PgPool;

/// The 32-byte key called `name`, created from `fresh` if there is none
/// yet. Concurrent first calls agree on one key.
pub async fn get_or_create(db: &PgPool, name: &str, fresh: [u8; 32]) -> sqlx::Result<[u8; 32]> {
    sqlx::query("INSERT INTO secrets (name, value) VALUES ($1, $2) ON CONFLICT (name) DO NOTHING")
        .bind(name)
        .bind(&fresh[..])
        .execute(db)
        .await?;
    let value: Vec<u8> = sqlx::query_scalar("SELECT value FROM secrets WHERE name = $1")
        .bind(name)
        .fetch_one(db)
        .await?;
    value
        .get(..32)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| sqlx::Error::Decode(format!("secret `{name}` is too short").into()))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn the_first_key_sticks(pool: PgPool) {
        let first = get_or_create(&pool, "k", [1; 32]).await.unwrap();
        let second = get_or_create(&pool, "k", [2; 32]).await.unwrap();
        assert_eq!(first, [1; 32]);
        assert_eq!(second, [1; 32]);
    }
}
