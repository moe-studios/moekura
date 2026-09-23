//! Queries on `roles`.

use sqlx::PgExecutor;
use uwuu_core::permissions::{Permissions, Role, SystemRole};

#[derive(sqlx::FromRow)]
struct RoleRow {
    id: i32,
    name: String,
    permissions: i64,
    rank: i16,
    system_key: Option<String>,
}

impl From<RoleRow> for Role {
    fn from(row: RoleRow) -> Self {
        Role {
            id: row.id,
            name: row.name,
            permissions: Permissions::from_db(row.permissions),
            rank: row.rank,
            system: row.system_key.as_deref().and_then(SystemRole::from_key),
        }
    }
}

/// `SELECT … FROM roles` followed by `$rest`, as a static string.
macro_rules! select_roles {
    ($rest:literal) => {
        concat!(
            "SELECT id, name::text, permissions, rank, system_key FROM roles ",
            $rest
        )
    };
}

/// Every role, lowest rank first.
pub async fn list(db: impl PgExecutor<'_>) -> sqlx::Result<Vec<Role>> {
    let rows: Vec<RoleRow> = sqlx::query_as(select_roles!("ORDER BY rank, id"))
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(Role::from).collect())
}

/// Renames a role and sets its permissions, telling every node's site
/// cache.
pub async fn update(
    db: &sqlx::PgPool,
    id: i32,
    name: &str,
    permissions: Permissions,
) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query("UPDATE roles SET name = $2, permissions = $3 WHERE id = $1")
        .bind(id)
        .bind(name)
        .bind(permissions.to_db())
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_notify($1, 'roles')")
        .bind(crate::site_cache::CHANNEL)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

pub async fn by_system(db: impl PgExecutor<'_>, role: SystemRole) -> sqlx::Result<Role> {
    let row: RoleRow = sqlx::query_as(select_roles!("WHERE system_key = $1"))
        .bind(role.key())
        .fetch_one(db)
        .await?;
    Ok(row.into())
}

/// Looks a role up by display name, case-insensitively. (The `::citext`
/// cast matters: a text parameter would make Postgres compare as text.)
pub async fn by_name(db: impl PgExecutor<'_>, name: &str) -> sqlx::Result<Option<Role>> {
    let row: Option<RoleRow> = sqlx::query_as(select_roles!("WHERE name = $1::citext"))
        .bind(name)
        .fetch_optional(db)
        .await?;
    Ok(row.map(Role::from))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn seeded_roles_match_code_defaults(pool: PgPool) {
        let roles = list(&pool).await.unwrap();
        let seeded: Vec<_> = roles.iter().map(|r| r.system.unwrap()).collect();
        assert_eq!(seeded, SystemRole::ALL);
        for role in roles {
            let system = role.system.unwrap();
            assert_eq!(role.permissions, system.default_permissions(), "{system:?}");
            assert_eq!(role.rank, system.default_rank(), "{system:?}");
        }
    }

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn finds_roles(pool: PgPool) {
        let admin = by_system(&pool, SystemRole::Admin).await.unwrap();
        assert_eq!(admin.permissions, Permissions::ALL);
        let by_display_name = by_name(&pool, "janitor").await.unwrap().unwrap();
        assert_eq!(by_display_name.system, Some(SystemRole::Janitor));
        assert!(by_name(&pool, "nobody").await.unwrap().is_none());
    }
}
