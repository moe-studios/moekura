//! Roles and the permissions they grant.
//!
//! A role's permissions are a 64-bit set stored as a Postgres `bigint`.
//! Each [`Permission`] owns a fixed bit, so **bit positions must never be
//! reused or reordered**; retire a permission by leaving its bit unused.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Something a user may be allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum Permission {
    /// See active posts, tags and wiki pages.
    ViewPosts = 0,
    Upload = 1,
    /// Change a post's tags, rating, source and parent.
    EditPosts = 2,
    Comment = 3,
    Favorite = 4,
    Vote = 5,
    Flag = 6,
    EditWiki = 7,
    /// Approve or reject posts in the moderation queue.
    ApprovePosts = 8,
    /// Soft-delete and restore posts.
    DeletePosts = 9,
    /// Permanently remove posts and their files.
    PurgePosts = 10,
    /// Manage tag aliases, implications and categories.
    ManageTags = 11,
    ViewDeleted = 12,
    BanUsers = 13,
    /// Change other users' roles and account status.
    ManageUsers = 14,
    /// Edit site settings and roles.
    ManageSettings = 15,
    ViewAuditLog = 16,
    /// Uploads skip the approval queue.
    UploadWithoutApproval = 17,
    /// Hide and restore anyone's comments, and settle reports about them.
    ModerateComments = 18,
    /// Create pools and change their posts, names and descriptions.
    EditPools = 19,
    /// Add, change and delete notes on posts.
    EditNotes = 20,
}

impl Permission {
    pub const ALL: [Permission; 21] = [
        Permission::ViewPosts,
        Permission::Upload,
        Permission::EditPosts,
        Permission::Comment,
        Permission::Favorite,
        Permission::Vote,
        Permission::Flag,
        Permission::EditWiki,
        Permission::ApprovePosts,
        Permission::DeletePosts,
        Permission::PurgePosts,
        Permission::ManageTags,
        Permission::ViewDeleted,
        Permission::BanUsers,
        Permission::ManageUsers,
        Permission::ManageSettings,
        Permission::ViewAuditLog,
        Permission::UploadWithoutApproval,
        Permission::ModerateComments,
        Permission::EditPools,
        Permission::EditNotes,
    ];

    const fn bit(self) -> u64 {
        1 << self as u8
    }

    /// Stable machine name, as in forms.
    pub fn key(self) -> &'static str {
        match self {
            Permission::ViewPosts => "view_posts",
            Permission::Upload => "upload",
            Permission::EditPosts => "edit_posts",
            Permission::Comment => "comment",
            Permission::Favorite => "favorite",
            Permission::Vote => "vote",
            Permission::Flag => "flag",
            Permission::EditWiki => "edit_wiki",
            Permission::ApprovePosts => "approve_posts",
            Permission::DeletePosts => "delete_posts",
            Permission::PurgePosts => "purge_posts",
            Permission::ManageTags => "manage_tags",
            Permission::ViewDeleted => "view_deleted",
            Permission::BanUsers => "ban_users",
            Permission::ManageUsers => "manage_users",
            Permission::ManageSettings => "manage_settings",
            Permission::ViewAuditLog => "view_audit_log",
            Permission::UploadWithoutApproval => "upload_without_approval",
            Permission::ModerateComments => "moderate_comments",
            Permission::EditPools => "edit_pools",
            Permission::EditNotes => "edit_notes",
        }
    }

    /// What admins see.
    pub fn label(self) -> &'static str {
        match self {
            Permission::ViewPosts => "View posts",
            Permission::Upload => "Upload",
            Permission::EditPosts => "Edit posts and tags",
            Permission::Comment => "Comment",
            Permission::Favorite => "Favorite",
            Permission::Vote => "Vote",
            Permission::Flag => "Flag posts",
            Permission::EditWiki => "Edit the wiki",
            Permission::ApprovePosts => "Approve posts and handle flags",
            Permission::DeletePosts => "Delete and restore posts",
            Permission::PurgePosts => "Purge posts",
            Permission::ManageTags => "Manage tags, aliases and implications",
            Permission::ViewDeleted => "See deleted posts",
            Permission::BanUsers => "Ban users and networks",
            Permission::ManageUsers => "Manage users",
            Permission::ManageSettings => "Manage site settings and roles",
            Permission::ViewAuditLog => "Read the moderation log",
            Permission::UploadWithoutApproval => "Upload without approval",
            Permission::ModerateComments => "Hide comments and handle reports about them",
            Permission::EditPools => "Create and edit pools",
            Permission::EditNotes => "Edit notes",
        }
    }
}

/// A set of [`Permission`]s.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Permissions(u64);

impl Permissions {
    pub const NONE: Self = Self(0);
    /// Every bit set, including bits for permissions added in the future.
    pub const ALL: Self = Self(u64::MAX);

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Reinterprets a Postgres `bigint` column.
    pub const fn from_db(value: i64) -> Self {
        Self(value as u64)
    }

    pub const fn to_db(self) -> i64 {
        self.0 as i64
    }

    pub const fn of(permissions: &[Permission]) -> Self {
        let mut bits = 0;
        let mut i = 0;
        while i < permissions.len() {
            bits |= permissions[i].bit();
            i += 1;
        }
        Self(bits)
    }

    pub const fn contains(self, permission: Permission) -> bool {
        self.0 & permission.bit() != 0
    }

    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn iter(self) -> impl Iterator<Item = Permission> {
        Permission::ALL
            .into_iter()
            .filter(move |p| self.contains(*p))
    }
}

impl fmt::Debug for Permissions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Self::ALL {
            return f.write_str("Permissions(ALL)");
        }
        f.debug_set().entries(self.iter()).finish()
    }
}

/// Code-level identity of a built-in role. Admins may rename built-in roles
/// or edit their permissions, but code finds them by this key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemRole {
    /// Applied to visitors who are not logged in.
    Anonymous,
    /// Given to newly registered accounts.
    Member,
    Contributor,
    Janitor,
    Moderator,
    Admin,
}

impl SystemRole {
    pub const ALL: [SystemRole; 6] = [
        SystemRole::Anonymous,
        SystemRole::Member,
        SystemRole::Contributor,
        SystemRole::Janitor,
        SystemRole::Moderator,
        SystemRole::Admin,
    ];

    pub const fn key(self) -> &'static str {
        match self {
            SystemRole::Anonymous => "anonymous",
            SystemRole::Member => "member",
            SystemRole::Contributor => "contributor",
            SystemRole::Janitor => "janitor",
            SystemRole::Moderator => "moderator",
            SystemRole::Admin => "admin",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.key() == key)
    }

    /// Higher ranks may act on users with lower ranks (ban, change role).
    pub const fn default_rank(self) -> i16 {
        match self {
            SystemRole::Anonymous => 0,
            SystemRole::Member => 10,
            SystemRole::Contributor => 20,
            SystemRole::Janitor => 30,
            SystemRole::Moderator => 40,
            SystemRole::Admin => 50,
        }
    }

    /// The permissions a fresh install seeds for this role. Each role
    /// includes everything the role below it can do.
    pub const fn default_permissions(self) -> Permissions {
        use Permission::*;
        const ANONYMOUS: Permissions = Permissions::of(&[ViewPosts]);
        const MEMBER: Permissions = ANONYMOUS.with(Permissions::of(&[
            Upload, EditPosts, Comment, Favorite, Vote, Flag, EditWiki, EditPools, EditNotes,
        ]));
        const CONTRIBUTOR: Permissions = MEMBER.with(Permissions::of(&[UploadWithoutApproval]));
        const JANITOR: Permissions = CONTRIBUTOR.with(Permissions::of(&[
            ApprovePosts,
            DeletePosts,
            ManageTags,
            ViewDeleted,
            ModerateComments,
        ]));
        const MODERATOR: Permissions = JANITOR.with(Permissions::of(&[BanUsers, ViewAuditLog]));
        match self {
            SystemRole::Anonymous => ANONYMOUS,
            SystemRole::Member => MEMBER,
            SystemRole::Contributor => CONTRIBUTOR,
            SystemRole::Janitor => JANITOR,
            SystemRole::Moderator => MODERATOR,
            SystemRole::Admin => Permissions::ALL,
        }
    }
}

/// A role as stored in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    pub id: i32,
    /// Display name; admins may change it.
    pub name: String,
    pub permissions: Permissions,
    pub rank: i16,
    pub system: Option<SystemRole>,
    pub upload_limits: crate::uploads::UploadLimits,
}

impl Role {
    pub const fn can(&self, permission: Permission) -> bool {
        self.permissions.contains(permission)
    }

    /// Whether holders of this role may act on holders of `other`.
    pub const fn outranks(&self, other: &Role) -> bool {
        self.rank > other.rank
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_are_stable() {
        // Changing these breaks every existing database. Add new
        // permissions at the end instead.
        assert_eq!(Permission::ViewPosts as u8, 0);
        assert_eq!(Permission::ManageSettings as u8, 15);
        assert_eq!(Permission::UploadWithoutApproval as u8, 17);
    }

    #[test]
    fn all_lists_every_permission_once() {
        let mut bits: Vec<u8> = Permission::ALL.iter().map(|p| *p as u8).collect();
        bits.sort_unstable();
        bits.dedup();
        assert_eq!(bits.len(), Permission::ALL.len());
        assert_eq!(*bits.last().unwrap() as usize, Permission::ALL.len() - 1);
    }

    #[test]
    fn set_operations() {
        let set = Permissions::of(&[Permission::Upload, Permission::Flag]);
        assert!(set.contains(Permission::Upload));
        assert!(!set.contains(Permission::ViewPosts));
        assert_eq!(
            set.iter().collect::<Vec<_>>(),
            [Permission::Upload, Permission::Flag]
        );
    }

    #[test]
    fn db_round_trip_preserves_all_bits() {
        assert_eq!(Permissions::ALL.to_db(), -1);
        assert_eq!(Permissions::from_db(-1), Permissions::ALL);
        let set = Permissions::of(&[Permission::ViewAuditLog]);
        assert_eq!(Permissions::from_db(set.to_db()), set);
    }

    #[test]
    fn default_roles_are_cumulative() {
        for pair in SystemRole::ALL.windows(2) {
            let (lower, higher) = (pair[0], pair[1]);
            let lower_set = lower.default_permissions();
            let higher_set = higher.default_permissions();
            assert_eq!(
                lower_set.with(higher_set),
                higher_set,
                "{higher:?} lacks some of {lower:?}"
            );
            assert!(higher.default_rank() > lower.default_rank());
        }
    }

    #[test]
    fn admin_gets_future_permissions() {
        assert!(
            SystemRole::Admin
                .default_permissions()
                .contains(Permission::PurgePosts)
        );
        assert_eq!(SystemRole::Admin.default_permissions(), Permissions::ALL);
    }

    #[test]
    fn anonymous_can_only_view() {
        let anonymous = SystemRole::Anonymous.default_permissions();
        assert_eq!(
            anonymous.iter().collect::<Vec<_>>(),
            [Permission::ViewPosts]
        );
    }

    #[test]
    fn system_role_keys_round_trip() {
        for role in SystemRole::ALL {
            assert_eq!(SystemRole::from_key(role.key()), Some(role));
        }
        assert_eq!(SystemRole::from_key("nope"), None);
    }
}
