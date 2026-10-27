//! Who may act on whom. Staff act on the accounts of people ranked below
//! them; the site's owner outranks every role. Nobody outranks themselves.

use moekura_db::site_cache::SiteSnapshot;
use moekura_db::users::User;

use crate::auth::CurrentUser;

/// Where someone stands when ranks are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Standing {
    /// Their role's rank.
    Rank(i16),
    /// The site's owner, above every role.
    Owner,
}

/// Where `user` stands: the owner, or their role's rank (0 if the role is
/// gone).
pub(crate) fn standing(site: &SiteSnapshot, user: &User) -> Standing {
    if user.is_owner {
        Standing::Owner
    } else {
        Standing::Rank(site.role(user.role_id).map_or(0, |r| r.rank))
    }
}

impl Standing {
    /// As a number for queries that compare ranks in SQL.
    pub(crate) fn as_rank(self) -> i16 {
        match self {
            Standing::Rank(rank) => rank,
            Standing::Owner => moekura_db::user_ips::OWNER_RANK,
        }
    }
}

impl CurrentUser {
    /// Where they stand: banned users stand as visitors do.
    pub(crate) fn standing(&self) -> Standing {
        match &self.user {
            Some(user) if user.is_owner && self.ban.is_none() => Standing::Owner,
            _ => Standing::Rank(self.role.rank),
        }
    }
}

/// Whether `current` outranks `target`. Nobody outranks themselves.
pub(crate) fn outranks(site: &SiteSnapshot, current: &CurrentUser, target: &User) -> bool {
    current.user.as_ref().is_some_and(|me| me.id != target.id)
        && current.standing() > standing(site, target)
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use moekura_db::users;

    use super::*;
    use crate::AppState;
    use crate::test_support::{session_for, test_state};

    async fn current(state: &AppState, name: &str) -> CurrentUser {
        let user = users::by_name(state.db.primary(), name)
            .await
            .unwrap()
            .unwrap();
        CurrentUser::for_user(user, None, &state.site.get())
    }

    #[sqlx::test(migrator = "moekura_db::MIGRATOR")]
    async fn ranks_and_ownership(pool: PgPool) {
        session_for(&pool, "janitor", SystemRole::Janitor).await;
        session_for(&pool, "janitor2", SystemRole::Janitor).await;
        session_for(&pool, "admin", SystemRole::Admin).await;
        session_for(&pool, "admin2", SystemRole::Admin).await;
        session_for(&pool, "owner", SystemRole::Admin).await;
        sqlx::query("UPDATE users SET is_owner = (name = 'owner')")
            .execute(&pool)
            .await
            .unwrap();
        let state = test_state(&pool).await;
        let site = state.site.get();
        let user = |name: &'static str| {
            let state = state.clone();
            async move {
                users::by_name(state.db.primary(), name)
                    .await
                    .unwrap()
                    .unwrap()
            }
        };
        let janitor = current(&state, "janitor").await;
        let admin = current(&state, "admin").await;
        let owner = current(&state, "owner").await;

        assert!(outranks(&site, &admin, &user("janitor").await));
        assert!(!outranks(&site, &janitor, &user("janitor2").await));
        assert!(!outranks(&site, &admin, &user("admin2").await));
        assert!(!outranks(&site, &admin, &user("admin").await));
        assert!(outranks(&site, &owner, &user("admin").await));
        assert!(!outranks(&site, &admin, &user("owner").await));
        assert!(!outranks(&site, &owner, &user("owner").await));
    }
}
