//! Background job payloads. Each job type has a stable `KIND` string stored
//! in the queue; renaming one strands queued jobs of the old name.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub trait Job: Serialize + DeserializeOwned + Send + 'static {
    const KIND: &'static str;
    /// Attempts before the job is marked dead.
    const MAX_ATTEMPTS: i32 = 5;
}

/// Generate thumbnails and samples and compute the perceptual hash of an
/// uploaded file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessMedia {
    pub asset_id: i64,
}

impl Job for ProcessMedia {
    const KIND: &'static str = "media.process";
}

/// Bring existing posts in line with a newly approved tag alias (replace
/// the antecedent) or implication (add the implied tags).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyTagRelation {
    pub relation_id: i32,
}

impl Job for ApplyTagRelation {
    const KIND: &'static str = "tags.apply_relation";
}

/// Remove a deleted post for good: its files, then its row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgePost {
    pub post_id: i64,
}

impl Job for PurgePost {
    const KIND: &'static str = "posts.purge";
}

/// Send an email (only queued when `mail` is configured).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMail {
    pub to: String,
    pub subject: String,
    /// Plain text.
    pub body: String,
}

impl Job for SendMail {
    const KIND: &'static str = "mail.send";
}

/// Promote members whose record meets the site's rules; scheduled hourly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoteUsers {}

impl Job for PromoteUsers {
    const KIND: &'static str = "users.promote";
    const MAX_ATTEMPTS: i32 = 3;
}
