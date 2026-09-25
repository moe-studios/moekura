//! Moderation vocabulary shared by the database and web layers.

/// Most characters in a moderation reason.
pub const REASON_MAX_LEN: usize = 2000;

/// What an audit log entry records. Stored by name; renaming one would
/// orphan old entries' labels, so only add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    PostApprove,
    PostReject,
    PostDelete,
    PostRestore,
    PostPurge,
    FlagDismiss,
    UserBan,
    UserUnban,
    IpBan,
    IpUnban,
    UserRole,
    UserStatus,
    UserTwoFactorReset,
    RoleUpdate,
    SettingUpdate,
    TagUpdate,
    TagRelationApprove,
    TagRelationReject,
    TagRelationRemove,
    JobRetry,
    JobDiscard,
    CommentHide,
    CommentRestore,
    CommentReportDismiss,
    PoolDelete,
    PoolUndelete,
    UserPromote,
}

impl ActionKind {
    pub const ALL: [ActionKind; 27] = [
        ActionKind::PostApprove,
        ActionKind::PostReject,
        ActionKind::PostDelete,
        ActionKind::PostRestore,
        ActionKind::PostPurge,
        ActionKind::FlagDismiss,
        ActionKind::UserBan,
        ActionKind::UserUnban,
        ActionKind::IpBan,
        ActionKind::IpUnban,
        ActionKind::UserRole,
        ActionKind::UserStatus,
        ActionKind::UserTwoFactorReset,
        ActionKind::RoleUpdate,
        ActionKind::SettingUpdate,
        ActionKind::TagUpdate,
        ActionKind::TagRelationApprove,
        ActionKind::TagRelationReject,
        ActionKind::TagRelationRemove,
        ActionKind::JobRetry,
        ActionKind::JobDiscard,
        ActionKind::CommentHide,
        ActionKind::CommentRestore,
        ActionKind::CommentReportDismiss,
        ActionKind::PoolDelete,
        ActionKind::PoolUndelete,
        ActionKind::UserPromote,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::PostApprove => "post.approve",
            ActionKind::PostReject => "post.reject",
            ActionKind::PostDelete => "post.delete",
            ActionKind::PostRestore => "post.restore",
            ActionKind::PostPurge => "post.purge",
            ActionKind::FlagDismiss => "flag.dismiss",
            ActionKind::UserBan => "user.ban",
            ActionKind::UserUnban => "user.unban",
            ActionKind::IpBan => "ip.ban",
            ActionKind::IpUnban => "ip.unban",
            ActionKind::UserRole => "user.role",
            ActionKind::UserStatus => "user.status",
            ActionKind::UserTwoFactorReset => "user.two_factor_reset",
            ActionKind::RoleUpdate => "role.update",
            ActionKind::SettingUpdate => "setting.update",
            ActionKind::TagUpdate => "tag.update",
            ActionKind::TagRelationApprove => "tag_relation.approve",
            ActionKind::TagRelationReject => "tag_relation.reject",
            ActionKind::TagRelationRemove => "tag_relation.remove",
            ActionKind::JobRetry => "job.retry",
            ActionKind::JobDiscard => "job.discard",
            ActionKind::CommentHide => "comment.hide",
            ActionKind::CommentRestore => "comment.restore",
            ActionKind::CommentReportDismiss => "comment_report.dismiss",
            ActionKind::PoolDelete => "pool.delete",
            ActionKind::PoolUndelete => "pool.undelete",
            ActionKind::UserPromote => "user.promote",
        }
    }

    /// How the log describes it.
    pub fn label(self) -> &'static str {
        match self {
            ActionKind::PostApprove => "approved post",
            ActionKind::PostReject => "rejected post",
            ActionKind::PostDelete => "deleted post",
            ActionKind::PostRestore => "restored post",
            ActionKind::PostPurge => "purged post",
            ActionKind::FlagDismiss => "dismissed flags on post",
            ActionKind::UserBan => "banned",
            ActionKind::UserUnban => "unbanned",
            ActionKind::IpBan => "banned addresses",
            ActionKind::IpUnban => "unbanned addresses",
            ActionKind::UserRole => "changed the role of",
            ActionKind::UserStatus => "changed the status of",
            ActionKind::UserTwoFactorReset => "turned off two-factor login for",
            ActionKind::RoleUpdate => "updated a role",
            ActionKind::SettingUpdate => "changed a site setting",
            ActionKind::TagUpdate => "edited a tag",
            ActionKind::TagRelationApprove => "approved a tag relation",
            ActionKind::TagRelationReject => "rejected a tag relation",
            ActionKind::TagRelationRemove => "removed a tag relation",
            ActionKind::JobRetry => "retried a job",
            ActionKind::JobDiscard => "discarded a job",
            ActionKind::CommentHide => "hid a comment on post",
            ActionKind::CommentRestore => "restored a comment on post",
            ActionKind::CommentReportDismiss => "dismissed reports about a comment on post",
            ActionKind::PoolDelete => "deleted a pool",
            ActionKind::PoolUndelete => "restored a pool",
            ActionKind::UserPromote => "promoted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_round_trip() {
        for kind in ActionKind::ALL {
            assert_eq!(ActionKind::parse(kind.as_str()), Some(kind));
        }
        let mut names: Vec<&str> = ActionKind::ALL.iter().map(|k| k.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ActionKind::ALL.len());
    }
}
