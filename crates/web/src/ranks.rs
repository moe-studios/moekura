//! Who may act on whom. Staff act on the accounts of people ranked below
//! them, and on what those people wrote or uploaded; the site's owner
//! outranks every role. Nobody outranks themselves, so staff may act
//! against their own work (delete their own upload) but not settle things
//! about it in their own favour (restore it, dismiss flags on it).

use moekura_db::site_cache::SiteSnapshot;
use moekura_db::users::{self, User};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::AppError;

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

/// Which way a moderation action goes for whoever wrote or uploaded what
/// it acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Act {
    /// Against them: deleting, hiding, rejecting.
    Against,
    /// For them: restoring, letting through, dismissing flags or reports,
    /// granting an appeal.
    For,
}

/// Whether `current` may moderate something by `owner` (`None` when the
/// account is gone): theirs only to act against, others' only if they
/// outrank them.
pub(crate) fn may_moderate(
    site: &SiteSnapshot,
    current: &CurrentUser,
    owner: Option<&User>,
    act: Act,
) -> bool {
    let Some(me) = current.user.as_ref() else {
        return false;
    };
    match owner {
        None => true,
        Some(owner) if owner.id == me.id => act == Act::Against,
        Some(owner) => current.standing() > standing(site, owner),
    }
}

/// [`may_moderate`] for something by user `owner_id`; the error says why
/// not.
pub(crate) async fn check_moderate(
    state: &AppState,
    current: &CurrentUser,
    owner_id: Option<i64>,
    act: Act,
) -> Result<(), AppError> {
    let owner = match owner_id {
        Some(id) => users::by_id(state.db.primary(), id).await?,
        None => None,
    };
    if may_moderate(&state.site.get(), current, owner.as_ref(), act) {
        return Ok(());
    }
    let own = owner
        .as_ref()
        .zip(current.user.as_ref())
        .is_some_and(|(owner, me)| owner.id == me.id);
    Err(AppError::Blocked(if own {
        "That's about your own work; leave it to other staff".into()
    } else {
        "That's by someone ranked at or above you; leave it to someone ranked above them".into()
    }))
}

#[cfg(test)]
mod tests {
    use moekura_core::permissions::SystemRole;
    use sqlx::PgPool;

    use super::*;
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

        let janitor_user = user("janitor").await;
        let mine = Some(&janitor_user);
        assert!(may_moderate(&site, &janitor, mine, Act::Against));
        assert!(!may_moderate(&site, &janitor, mine, Act::For));
        assert!(may_moderate(&site, &janitor, None, Act::For));
        let peer = user("janitor2").await;
        assert!(!may_moderate(&site, &janitor, Some(&peer), Act::Against));
        assert!(may_moderate(&site, &admin, Some(&peer), Act::For));
        let admin2 = user("admin2").await;
        assert!(!may_moderate(&site, &admin, Some(&admin2), Act::Against));
        assert!(may_moderate(&site, &owner, Some(&admin2), Act::Against));
        let visitor = CurrentUser::anonymous(&site);
        assert!(!may_moderate(&site, &visitor, None, Act::Against));
    }
}
