//! Queries on `roles`.

use moekura_core::permissions::{Permissions, Role, SystemRole};
use moekura_core::uploads::UploadLimits;
use sqlx::PgExecutor;

#[derive(sqlx::FromRow)]
struct RoleRow {
    id: i32,
    name: String,
    permissions: i64,
    rank: i16,
    system_key: Option<String>,
    pending_upload_limit: Option<i32>,
    daily_upload_limit: Option<i32>,
}

impl From<RoleRow> for Role {
    fn from(row: RoleRow) -> Self {
        Role {
            id: row.id,
            name: row.name,
            permissions: Permissions::from_db(row.permissions),
            rank: row.rank,
            system: row.system_key.as_deref().and_then(SystemRole::from_key),
            upload_limits: UploadLimits {
                pending: row.pending_upload_limit,
                daily: row.daily_upload_limit,
            },
        }
    }
}

/// `SELECT … FROM roles` followed by `$rest`, as a static string.
macro_rules! select_roles {
    ($rest:literal) => {
        concat!(
            "SELECT id, name::text, permissions, rank, system_key, pending_upload_limit,
                    daily_upload_limit
             FROM roles ",
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

/// Tells every node's site cache that roles changed, once the caller's
/// transaction commits.
async fn notify(conn: &mut sqlx::PgConnection) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_notify($1, 'roles')")
        .bind(crate::site_cache::CHANNEL)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Renames a role and sets its permissions and upload limits.
pub async fn update(
    conn: &mut sqlx::PgConnection,
    id: i32,
    name: &str,
    permissions: Permissions,
    limits: UploadLimits,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE roles SET name = $2, permissions = $3, pending_upload_limit = $4,
                          daily_upload_limit = $5
         WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(permissions.to_db())
    .bind(limits.pending)
    .bind(limits.daily)
    .execute(&mut *conn)
    .await?;
    notify(conn).await
}

/// Adds a role (never a built-in one) and returns its id.
pub async fn create(
    conn: &mut sqlx::PgConnection,
    name: &str,
    rank: i16,
    permissions: Permissions,
) -> sqlx::Result<i32> {
    let id = sqlx::query_scalar(
        "INSERT INTO roles (name, rank, permissions) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(name)
    .bind(rank)
    .bind(permissions.to_db())
    .fetch_one(&mut *conn)
    .await?;
    notify(conn).await?;
    Ok(id)
}

/// Changes the rank of custom role `id`; false if it's built in or gone.
/// Code relies on the built-in roles' order, so their ranks stay.
pub async fn set_rank(conn: &mut sqlx::PgConnection, id: i32, rank: i16) -> sqlx::Result<bool> {
    let changed = sqlx::query("UPDATE roles SET rank = $2 WHERE id = $1 AND system_key IS NULL")
        .bind(id)
        .bind(rank)
        .execute(&mut *conn)
        .await?
        .rows_affected()
        == 1;
    if changed {
        notify(conn).await?;
    }
    Ok(changed)
}

/// Deletes custom role `id`, first moving its users to role `move_to`.
/// Returns how many users moved, or `None` if the role is built in or
/// gone.
pub async fn delete(
    conn: &mut sqlx::PgConnection,
    id: i32,
    move_to: i32,
) -> sqlx::Result<Option<u64>> {
    let custom: Option<i32> =
        sqlx::query_scalar("SELECT id FROM roles WHERE id = $1 AND system_key IS NULL FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    if custom.is_none() {
        return Ok(None);
    }
    let moved = sqlx::query("UPDATE users SET role_id = $2 WHERE role_id = $1")
        .bind(id)
        .bind(move_to)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM roles WHERE id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    notify(conn).await?;
    Ok(Some(moved))
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

    #[sqlx::test(migrator = "crate::MIGRATOR")]
    async fn custom_roles(pool: PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let member = by_system(&pool, SystemRole::Member).await.unwrap();
        let id = create(&mut conn, "Trusted", 15, member.permissions)
            .await
            .unwrap();
        let trusted = by_name(&pool, "trusted").await.unwrap().unwrap();
        assert_eq!((trusted.id, trusted.rank, trusted.system), (id, 15, None));
        assert!(set_rank(&mut conn, id, 25).await.unwrap());
        assert!(
            !set_rank(&mut conn, member.id, 25).await.unwrap(),
            "built-in ranks stay"
        );
        assert_eq!(by_name(&pool, "member").await.unwrap().unwrap().rank, 10);

        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (name, role_id) VALUES ('alice', $1) RETURNING id",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(delete(&mut conn, member.id, id).await.unwrap(), None);
        assert_eq!(delete(&mut conn, id, member.id).await.unwrap(), Some(1));
        assert!(by_name(&pool, "trusted").await.unwrap().is_none());
        let role_id: i32 = sqlx::query_scalar("SELECT role_id FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(role_id, member.id);
    }
}
