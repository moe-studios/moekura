//! Moderation vocabulary shared by the database and web layers.

/// Most characters in a moderation reason.
pub const REASON_MAX_LEN: usize = 2000;

/// Most characters in a preset reason (see [`PostReasons`]).
pub const PRESET_MAX_LEN: usize = 200;

/// Most preset reasons in each list.
pub const MAX_PRESETS: usize = 50;

/// Reasons offered when deleting, rejecting or flagging a post, so
/// they're consistent; a free-text reason is always possible too.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PostReasons {
    /// For deleting and rejecting posts.
    pub deletion: Vec<String>,
    /// For flagging posts.
    pub flag: Vec<String>,
}

impl Default for PostReasons {
    fn default() -> Self {
        let common = ["Duplicate", "Poor quality", "Off-topic", "Breaks the rules"];
        Self {
            deletion: common.map(String::from).to_vec(),
            flag: common.map(String::from).to_vec(),
        }
    }
}

impl PostReasons {
    /// Reasons from text, one per line, trimmed, without blanks or
    /// repeats.
    pub fn parse_list(text: &str) -> Vec<String> {
        let mut reasons: Vec<String> = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if !reasons.iter().any(|r| r == line) {
                reasons.push(line.to_owned());
            }
        }
        reasons
    }

    pub fn validate(&self) -> Result<(), String> {
        for list in [&self.deletion, &self.flag] {
            if list.len() > MAX_PRESETS {
                return Err(format!("offer at most {MAX_PRESETS} reasons of each kind"));
            }
            if list
                .iter()
                .any(|r| r.trim().is_empty() || r.chars().count() > PRESET_MAX_LEN)
            {
                return Err(format!("each reason has 1 to {PRESET_MAX_LEN} characters"));
            }
        }
        Ok(())
    }
}

/// The reason given with a preset (`preset`) and free text (`text`): the
/// preset, the text, or both as "preset: text".
pub fn combine_reason(preset: &str, text: &str) -> String {
    match (preset.trim(), text.trim()) {
        (preset, "") => preset.to_owned(),
        ("", text) => text.to_owned(),
        (preset, text) => format!("{preset}: {text}"),
    }
}

/// Why an approver passed on a pending post without rejecting it
/// (Danbooru's disapprovals).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisapprovalReason {
    BreaksRules,
    PoorQuality,
    /// The approver just isn't interested in approving it.
    Disinterest,
}

impl DisapprovalReason {
    pub const ALL: [DisapprovalReason; 3] = [
        DisapprovalReason::BreaksRules,
        DisapprovalReason::PoorQuality,
        DisapprovalReason::Disinterest,
    ];

    /// As stored, and as Danbooru names them.
    pub fn as_str(self) -> &'static str {
        match self {
            DisapprovalReason::BreaksRules => "breaks_rules",
            DisapprovalReason::PoorQuality => "poor_quality",
            DisapprovalReason::Disinterest => "disinterest",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DisapprovalReason::BreaksRules => "Breaks the rules",
            DisapprovalReason::PoorQuality => "Poor quality",
            DisapprovalReason::Disinterest => "Not interested",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

/// The longest timed ban, in days; longer ones are until lifted.
pub const MAX_BAN_DAYS: i64 = 3650;

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
    CommentSticky,
    CommentUnsticky,
    PoolDelete,
    PoolUndelete,
    UserPromote,
    MassUpdate,
    BulkUpdateApprove,
    BulkUpdateReject,
    RoleCreate,
    RoleDelete,
    AppealReject,
    PostLock,
    UndoEdits,
    DeleteUploads,
    PurgePosts,
}

impl ActionKind {
    pub const ALL: [ActionKind; 39] = [
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
        ActionKind::CommentSticky,
        ActionKind::CommentUnsticky,
        ActionKind::PoolDelete,
        ActionKind::PoolUndelete,
        ActionKind::UserPromote,
        ActionKind::MassUpdate,
        ActionKind::BulkUpdateApprove,
        ActionKind::BulkUpdateReject,
        ActionKind::RoleCreate,
        ActionKind::RoleDelete,
        ActionKind::AppealReject,
        ActionKind::PostLock,
        ActionKind::UndoEdits,
        ActionKind::DeleteUploads,
        ActionKind::PurgePosts,
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
            ActionKind::CommentSticky => "comment.sticky",
            ActionKind::CommentUnsticky => "comment.unsticky",
            ActionKind::PoolDelete => "pool.delete",
            ActionKind::PoolUndelete => "pool.undelete",
            ActionKind::UserPromote => "user.promote",
            ActionKind::MassUpdate => "tags.mass_update",
            ActionKind::BulkUpdateApprove => "bulk_update.approve",
            ActionKind::BulkUpdateReject => "bulk_update.reject",
            ActionKind::RoleCreate => "role.create",
            ActionKind::RoleDelete => "role.delete",
            ActionKind::AppealReject => "appeal.reject",
            ActionKind::PostLock => "post.lock",
            ActionKind::UndoEdits => "post_versions.undo",
            ActionKind::DeleteUploads => "posts.delete_uploads",
            ActionKind::PurgePosts => "posts.purge_batch",
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
            ActionKind::CommentSticky => "pinned a comment on post",
            ActionKind::CommentUnsticky => "unpinned a comment on post",
            ActionKind::PoolDelete => "deleted a pool",
            ActionKind::PoolUndelete => "restored a pool",
            ActionKind::UserPromote => "promoted",
            ActionKind::MassUpdate => "started a mass tag edit",
            ActionKind::BulkUpdateApprove => "approved a bulk update request",
            ActionKind::BulkUpdateReject => "rejected a bulk update request",
            ActionKind::RoleCreate => "added a role",
            ActionKind::RoleDelete => "deleted a role",
            ActionKind::AppealReject => "turned down an appeal of post",
            ActionKind::PostLock => "changed the locks on post",
            ActionKind::UndoEdits => "undid the post edits of",
            ActionKind::DeleteUploads => "started deleting the uploads of",
            ActionKind::PurgePosts => "started purging deleted posts",
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
    fn reasons_combine() {
        assert_eq!(combine_reason("Duplicate", " "), "Duplicate");
        assert_eq!(combine_reason("", "blurry"), "blurry");
        assert_eq!(
            combine_reason("Poor quality", "blurry"),
            "Poor quality: blurry"
        );
        assert_eq!(PostReasons::parse_list(" a \n\nb\na\n"), ["a", "b"]);
        assert!(PostReasons::default().validate().is_ok());
        let long = PostReasons {
            deletion: vec!["x".repeat(PRESET_MAX_LEN + 1)],
            flag: Vec::new(),
        };
        assert!(long.validate().is_err());
    }

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
