//! Where something written stands: a comment, a forum post or a copy of
//! a message.

/// Where a comment, forum post or message stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Status {
    /// Out for everyone who may see it.
    Active,
    /// Deleted by whoever wrote it, or for messages the copy's owner.
    Deleted,
    /// Hidden by staff.
    Hidden,
    /// Held as likely spam until the staff approve or reject it.
    Held,
    /// A held message the staff rejected: kept, never delivered.
    Rejected,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Deleted => "deleted",
            Status::Hidden => "hidden",
            Status::Held => "held",
            Status::Rejected => "rejected",
        }
    }
}
