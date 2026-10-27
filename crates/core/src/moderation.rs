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

/// Posts left waiting too long, as Danbooru deletes them: pending posts
/// nobody approved, and flagged posts whose flags nobody dealt with, in
/// that many days. 0 leaves them waiting.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StalePosts {
    pub pending_days: u32,
    pub flagged_days: u32,
}

/// The longest [`StalePosts`] wait.
pub const MAX_STALE_DAYS: u32 = 365;

impl StalePosts {
    pub fn validate(&self) -> Result<(), String> {
        if self.pending_days > MAX_STALE_DAYS || self.flagged_days > MAX_STALE_DAYS {
            return Err(format!(
                "posts wait at most {MAX_STALE_DAYS} days before they're deleted"
            ));
        }
        Ok(())
    }

    /// The reason a pending post deleted this way shows.
    pub fn pending_reason(&self) -> String {
        format!("Unapproved in {} days", self.pending_days)
    }

    /// The reason a flagged post deleted this way shows.
    pub fn flagged_reason(&self) -> String {
        format!("Flagged and not reviewed in {} days", self.flagged_days)
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

/// What a log entry is about besides a post or a user, by kind; stored by
/// name with the thing's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubjectKind {
    Comment,
    ForumPost,
    ForumTopic,
    Dmail,
    UserNote,
    UserFeedback,
    Pool,
    Artist,
    News,
    WikiPage,
    Note,
}

impl SubjectKind {
    pub const ALL: [SubjectKind; 11] = [
        SubjectKind::Comment,
        SubjectKind::ForumPost,
        SubjectKind::ForumTopic,
        SubjectKind::Dmail,
        SubjectKind::UserNote,
        SubjectKind::UserFeedback,
        SubjectKind::Pool,
        SubjectKind::Artist,
        SubjectKind::News,
        SubjectKind::WikiPage,
        SubjectKind::Note,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SubjectKind::Comment => "comment",
            SubjectKind::ForumPost => "forum_post",
            SubjectKind::ForumTopic => "forum_topic",
            SubjectKind::Dmail => "dmail",
            SubjectKind::UserNote => "user_note",
            SubjectKind::UserFeedback => "user_feedback",
            SubjectKind::Pool => "pool",
            SubjectKind::Artist => "artist",
            SubjectKind::News => "news",
            SubjectKind::WikiPage => "wiki_page",
            SubjectKind::Note => "note",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// What members can report to staff, besides posts (flags) and comments
/// (comment reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportKind {
    /// A private message, by its recipient.
    Dmail,
    ForumPost,
    User,
    WikiPage,
    Pool,
    Note,
    Artist,
    /// Feedback someone left on a user.
    UserFeedback,
}

impl ReportKind {
    pub const ALL: [ReportKind; 8] = [
        ReportKind::Dmail,
        ReportKind::ForumPost,
        ReportKind::User,
        ReportKind::WikiPage,
        ReportKind::Pool,
        ReportKind::Note,
        ReportKind::Artist,
        ReportKind::UserFeedback,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReportKind::Dmail => "dmail",
            ReportKind::ForumPost => "forum_post",
            ReportKind::User => "user",
            ReportKind::WikiPage => "wiki_page",
            ReportKind::Pool => "pool",
            ReportKind::Note => "note",
            ReportKind::Artist => "artist",
            ReportKind::UserFeedback => "user_feedback",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Who handles reports of this kind.
    pub fn handled_by(self) -> crate::permissions::Permission {
        use crate::permissions::Permission;
        match self {
            ReportKind::Dmail => Permission::ModerateMessages,
            ReportKind::User | ReportKind::UserFeedback => Permission::BanUsers,
            _ => Permission::ModerateComments,
        }
    }

    /// How the log knows it, when it isn't a user (the log's user column).
    pub fn subject(self) -> Option<SubjectKind> {
        match self {
            ReportKind::Dmail => Some(SubjectKind::Dmail),
            ReportKind::ForumPost => Some(SubjectKind::ForumPost),
            ReportKind::User => None,
            ReportKind::WikiPage => Some(SubjectKind::WikiPage),
            ReportKind::Pool => Some(SubjectKind::Pool),
            ReportKind::Note => Some(SubjectKind::Note),
            ReportKind::Artist => Some(SubjectKind::Artist),
            ReportKind::UserFeedback => Some(SubjectKind::UserFeedback),
        }
    }
}

/// Most characters of a reported thing's text kept with a report.
pub const SNAPSHOT_MAX_LEN: usize = 50_000;

/// Days before someone may report the same thing again once their report
/// was settled, and appeal or flag again once turned down.
pub const REPORT_COOLDOWN_DAYS: i64 = 7;

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
    ArtistBan,
    ArtistUnban,
    PostReplace,
    DeleteUploads,
    PurgePosts,
    DmailReportSettle,
    ForumTopicModerate,
    ForumTopicMerge,
    ForumPostHide,
    ForumPostUnhide,
    HeldApprove,
    HeldReject,
    FeedbackDelete,
    FeedbackRestore,
    UserRename,
    UserProfileClear,
    NewsPost,
    NewsDelete,
    TagCategoryCreate,
    TagCategoryUpdate,
    TagCategoryMove,
    TagCategoryDelete,
    UserOwner,
    UserNoteDelete,
    ForumPostEdit,
    ForumTopicEdit,
    FlagCreate,
    AppealCreate,
    PostDisapprove,
    FeedbackCreate,
    FeedbackUpdate,
    ReportCreate,
    ReportSettle,
    PostRemoveOldFile,
    PostUndisapprove,
    BanAppealCreate,
    BanAppealGrant,
    BanAppealDeny,
}

impl ActionKind {
    pub const ALL: [ActionKind; 75] = [
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
        ActionKind::ArtistBan,
        ActionKind::ArtistUnban,
        ActionKind::PostReplace,
        ActionKind::DeleteUploads,
        ActionKind::PurgePosts,
        ActionKind::DmailReportSettle,
        ActionKind::ForumTopicModerate,
        ActionKind::ForumTopicMerge,
        ActionKind::ForumPostHide,
        ActionKind::ForumPostUnhide,
        ActionKind::HeldApprove,
        ActionKind::HeldReject,
        ActionKind::FeedbackDelete,
        ActionKind::FeedbackRestore,
        ActionKind::UserRename,
        ActionKind::UserProfileClear,
        ActionKind::NewsPost,
        ActionKind::NewsDelete,
        ActionKind::TagCategoryCreate,
        ActionKind::TagCategoryUpdate,
        ActionKind::TagCategoryMove,
        ActionKind::TagCategoryDelete,
        ActionKind::UserOwner,
        ActionKind::UserNoteDelete,
        ActionKind::ForumPostEdit,
        ActionKind::ForumTopicEdit,
        ActionKind::FlagCreate,
        ActionKind::AppealCreate,
        ActionKind::PostDisapprove,
        ActionKind::FeedbackCreate,
        ActionKind::FeedbackUpdate,
        ActionKind::ReportCreate,
        ActionKind::ReportSettle,
        ActionKind::PostRemoveOldFile,
        ActionKind::PostUndisapprove,
        ActionKind::BanAppealCreate,
        ActionKind::BanAppealGrant,
        ActionKind::BanAppealDeny,
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
            ActionKind::IpBan => "network.ban",
            ActionKind::IpUnban => "network.unban",
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
            ActionKind::PoolUndelete => "pool.restore",
            ActionKind::UserPromote => "user.promote",
            ActionKind::MassUpdate => "tag.mass_update",
            ActionKind::BulkUpdateApprove => "bulk_update.approve",
            ActionKind::BulkUpdateReject => "bulk_update.reject",
            ActionKind::RoleCreate => "role.create",
            ActionKind::RoleDelete => "role.delete",
            ActionKind::AppealReject => "appeal.reject",
            ActionKind::PostLock => "post.lock",
            ActionKind::UndoEdits => "user.undo_edits",
            ActionKind::ArtistBan => "artist.ban",
            ActionKind::ArtistUnban => "artist.unban",
            ActionKind::PostReplace => "post.replace",
            ActionKind::DeleteUploads => "user.delete_uploads",
            ActionKind::PurgePosts => "post.purge_batch",
            ActionKind::DmailReportSettle => "dmail_report.settle",
            ActionKind::ForumTopicModerate => "forum_topic.moderate",
            ActionKind::ForumTopicMerge => "forum_topic.merge",
            ActionKind::ForumPostHide => "forum_post.hide",
            ActionKind::ForumPostUnhide => "forum_post.restore",
            ActionKind::HeldApprove => "held.approve",
            ActionKind::HeldReject => "held.reject",
            ActionKind::FeedbackDelete => "user_feedback.delete",
            ActionKind::FeedbackRestore => "user_feedback.restore",
            ActionKind::UserRename => "user.rename",
            ActionKind::UserProfileClear => "user.profile_clear",
            ActionKind::NewsPost => "news.post",
            ActionKind::NewsDelete => "news.delete",
            ActionKind::TagCategoryCreate => "tag_category.create",
            ActionKind::TagCategoryUpdate => "tag_category.update",
            ActionKind::TagCategoryMove => "tag_category.move",
            ActionKind::TagCategoryDelete => "tag_category.delete",
            ActionKind::UserOwner => "user.owner",
            ActionKind::UserNoteDelete => "user_note.delete",
            ActionKind::ForumPostEdit => "forum_post.edit",
            ActionKind::ForumTopicEdit => "forum_topic.edit",
            ActionKind::FlagCreate => "flag.create",
            ActionKind::AppealCreate => "appeal.create",
            ActionKind::PostDisapprove => "post.disapprove",
            ActionKind::FeedbackCreate => "user_feedback.create",
            ActionKind::FeedbackUpdate => "user_feedback.update",
            ActionKind::ReportCreate => "report.create",
            ActionKind::ReportSettle => "report.settle",
            ActionKind::PostRemoveOldFile => "post.remove_old_file",
            ActionKind::PostUndisapprove => "post.undisapprove",
            ActionKind::BanAppealCreate => "ban_appeal.create",
            ActionKind::BanAppealGrant => "ban_appeal.grant",
            ActionKind::BanAppealDeny => "ban_appeal.deny",
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
            ActionKind::ArtistBan => "banned an artist",
            ActionKind::ArtistUnban => "unbanned an artist",
            ActionKind::PostReplace => "replaced the file of post",
            ActionKind::DeleteUploads => "started deleting the uploads of",
            ActionKind::PurgePosts => "started purging deleted posts",
            ActionKind::DmailReportSettle => "settled a report about a message from",
            ActionKind::ForumTopicModerate => "changed a forum topic",
            ActionKind::ForumTopicMerge => "merged forum topics",
            ActionKind::ForumPostHide => "hid a forum post by",
            ActionKind::ForumPostUnhide => "restored a forum post by",
            ActionKind::HeldApprove => "let through writing held as spam, by",
            ActionKind::HeldReject => "turned away writing held as spam, by",
            ActionKind::FeedbackDelete => "deleted feedback on",
            ActionKind::FeedbackRestore => "restored feedback on",
            ActionKind::UserRename => "renamed",
            ActionKind::UserProfileClear => "cleared the profile of",
            ActionKind::NewsPost => "posted site news",
            ActionKind::NewsDelete => "deleted site news",
            ActionKind::TagCategoryCreate => "added a tag category",
            ActionKind::TagCategoryUpdate => "renamed a tag category",
            ActionKind::TagCategoryMove => "reordered the tag categories",
            ActionKind::TagCategoryDelete => "deleted a tag category",
            ActionKind::UserOwner => "handed the site's ownership to",
            ActionKind::UserNoteDelete => "deleted a staff note about",
            ActionKind::ForumPostEdit => "changed a forum post by",
            ActionKind::ForumTopicEdit => "renamed or moved a forum topic by",
            ActionKind::FlagCreate => "flagged post",
            ActionKind::AppealCreate => "appealed post",
            ActionKind::PostDisapprove => "passed on post",
            ActionKind::FeedbackCreate => "left feedback on",
            ActionKind::FeedbackUpdate => "changed feedback on",
            ActionKind::ReportCreate => "made a report about",
            ActionKind::ReportSettle => "settled reports about",
            ActionKind::PostRemoveOldFile => "removed an earlier file of post",
            ActionKind::PostUndisapprove => "took back passing on post",
            ActionKind::BanAppealCreate => "appealed the ban of",
            ActionKind::BanAppealGrant => "granted the ban appeal of",
            ActionKind::BanAppealDeny => "turned down the ban appeal of",
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
