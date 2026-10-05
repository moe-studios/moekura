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

/// Remove the square thumbnails (`crop-<size>` variants) earlier versions
/// made, files and all; queued once by the migration that dropped them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveSquareThumbnails {}

impl Job for RemoveSquareThumbnails {
    const KIND: &'static str = "media.remove_square_thumbnails";
}

/// Hash the pixels of every still image posted before pixel hashes were
/// kept; queued once by the migration that added them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashPixels {}

impl Job for HashPixels {
    const KIND: &'static str = "media.hash_pixels";
}

/// Recompute every artist URL's normalized form, and encode what a stored
/// URL can't hold raw, after the way URLs are compared or kept changed;
/// queued by the migration that changed it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizeArtistUrls {}

impl Job for NormalizeArtistUrls {
    const KIND: &'static str = "artists.normalize_urls";
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

/// Work through a moderation of many posts (`post_batches` row `id`),
/// such as deleting every upload of a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostBatch {
    pub id: i64,
}

impl Job for PostBatch {
    const KIND: &'static str = "posts.batch";
    /// Retries carry on where the last attempt stopped.
    const MAX_ATTEMPTS: i32 = 3;
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

/// Forget the addresses accounts haven't used for longer than the
/// `ip_history_days` site setting; scheduled daily.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneIpHistory {}

impl Job for PruneIpHistory {
    const KIND: &'static str = "users.prune_ips";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Count site statistics: the last two days' activity (or, the first
/// time, the last year's) and the site-wide totals; scheduled hourly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshStats {}

impl Job for RefreshStats {
    const KIND: &'static str = "stats.refresh";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Undo the post edits user `user_id` made between `since` and `until`
/// (Unix times; either open-ended), credited to `actor_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoUserEdits {
    pub user_id: i64,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub actor_id: Option<i64>,
}

impl Job for UndoUserEdits {
    const KIND: &'static str = "post_versions.undo_user";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Apply a mass tag edit (`mass_updates` row `id`) to every post matching
/// its search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MassUpdate {
    pub id: i64,
}

impl Job for MassUpdate {
    const KIND: &'static str = "tags.mass_update";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Apply an approved bulk update request's commands, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyBulkUpdate {
    pub request_id: i32,
}

impl Job for ApplyBulkUpdate {
    const KIND: &'static str = "tags.bulk_update";
    /// Commands already applied are safe to repeat, but a failure is
    /// reported rather than retried endlessly.
    const MAX_ATTEMPTS: i32 = 2;
}

/// Remove staged uploads nobody made into a post; scheduled hourly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpireStagedUploads {}

impl Job for ExpireStagedUploads {
    const KIND: &'static str = "uploads.expire_staged";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Send webhook delivery `delivery_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverWebhook {
    pub delivery_id: i64,
}

impl Job for DeliverWebhook {
    const KIND: &'static str = "webhooks.deliver";
    /// With the queue's backoff (10 s doubling to an hour), about five
    /// hours of retries.
    const MAX_ATTEMPTS: i32 = 12;
}

/// Remove webhook deliveries older than a month; scheduled daily.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneWebhookDeliveries {}

impl Job for PruneWebhookDeliveries {
    const KIND: &'static str = "webhooks.prune";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Suggest tags for a post with the tagger, once its thumbnails exist.
/// Only `moekura tagger` takes these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagPost {
    pub post_id: i64,
}

impl Job for TagPost {
    const KIND: &'static str = "ml.tag_post";
    const MAX_ATTEMPTS: i32 = 3;
}

/// Suggest tags for a staged upload's file, for its post form. Only
/// `moekura tagger` takes these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagStaged {
    pub staged_id: i64,
}

impl Job for TagStaged {
    const KIND: &'static str = "ml.tag_staged";
    const MAX_ATTEMPTS: i32 = 3;
}
