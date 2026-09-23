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
