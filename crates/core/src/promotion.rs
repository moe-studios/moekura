//! Promoting members to contributors automatically, once their record
//! meets the site's rules.

use serde::{Deserialize, Serialize};

/// What a member needs to be promoted, from the site settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    /// Uploads approved (active or flagged).
    pub uploads: u32,
    /// Post edits (versions after the upload).
    pub edits: u32,
    /// Days since registering.
    pub account_days: u32,
    /// Most of their uploads deleted in the last 30 days.
    pub max_recent_deletions: u32,
}

/// A member's record, as the rules look at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Record {
    pub uploads: i64,
    pub edits: i64,
    pub account_days: i64,
    pub recent_deletions: i64,
}

impl Rules {
    pub fn met_by(&self, record: &Record) -> bool {
        record.uploads >= i64::from(self.uploads)
            && record.edits >= i64::from(self.edits)
            && record.account_days >= i64::from(self.account_days)
            && record.recent_deletions <= i64::from(self.max_recent_deletions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        let rules = Rules {
            uploads: 50,
            edits: 10,
            account_days: 30,
            max_recent_deletions: 1,
        };
        let good = Record {
            uploads: 50,
            edits: 10,
            account_days: 30,
            recent_deletions: 1,
        };
        assert!(rules.met_by(&good));
        assert!(!rules.met_by(&Record {
            uploads: 49,
            ..good
        }));
        assert!(!rules.met_by(&Record { edits: 9, ..good }));
        assert!(!rules.met_by(&Record {
            account_days: 29,
            ..good
        }));
        assert!(!rules.met_by(&Record {
            recent_deletions: 2,
            ..good
        }));
    }
}
