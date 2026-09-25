//! Upload limits.

/// A role's upload limits; `None` is no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UploadLimits {
    /// Uploads waiting for approval at once.
    pub pending: Option<i32>,
    /// Uploads in the last 24 hours.
    pub daily: Option<i32>,
}

/// A user's uploads, as limits count them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UploadCounts {
    pub pending: i64,
    /// Uploaded in the last 24 hours.
    pub today: i64,
    /// Approved (active or flagged) uploads, ever.
    pub approved: i64,
    /// Deleted uploads, ever.
    pub deleted: i64,
}

/// The pending limit for a user with `counts`: the role's `base`, or with
/// `scaling`, one more per 10 approved uploads and one fewer per 5
/// deleted ones, from 1 up to 4 × `base` (as on Danbooru).
pub fn pending_limit(base: i32, counts: &UploadCounts, scaling: bool) -> i64 {
    let base = i64::from(base);
    if !scaling || base == 0 {
        return base;
    }
    (base + counts.approved / 10 - counts.deleted / 5).clamp(1, base * 4)
}

/// Why another upload isn't allowed now, if it isn't. `pending_applies`
/// says whether the upload would wait for approval.
pub fn refusal(
    limits: &UploadLimits,
    counts: &UploadCounts,
    scaling: bool,
    pending_applies: bool,
) -> Option<String> {
    if let Some(daily) = limits.daily
        && counts.today >= i64::from(daily)
    {
        return Some(format!(
            "You've reached your limit of {daily} uploads a day. Try again later."
        ));
    }
    if pending_applies && let Some(base) = limits.pending {
        let limit = pending_limit(base, counts, scaling);
        if counts.pending >= limit {
            return Some(format!(
                "You have {} uploads waiting for approval, your limit. Upload more once some are approved.",
                counts.pending
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(pending: i64, today: i64, approved: i64, deleted: i64) -> UploadCounts {
        UploadCounts {
            pending,
            today,
            approved,
            deleted,
        }
    }

    #[test]
    fn pending_limit_scales_with_the_record() {
        assert_eq!(pending_limit(10, &counts(0, 0, 100, 0), false), 10);
        assert_eq!(pending_limit(10, &counts(0, 0, 100, 0), true), 20);
        assert_eq!(pending_limit(10, &counts(0, 0, 1000, 0), true), 40);
        assert_eq!(pending_limit(10, &counts(0, 0, 0, 100), true), 1);
        assert_eq!(pending_limit(0, &counts(0, 0, 100, 0), true), 0);
    }

    #[test]
    fn refusals() {
        let limits = UploadLimits {
            pending: Some(2),
            daily: Some(5),
        };
        assert_eq!(refusal(&limits, &counts(1, 4, 0, 0), false, true), None);
        assert!(
            refusal(&limits, &counts(2, 0, 0, 0), false, true)
                .unwrap()
                .contains("waiting")
        );
        // Uploads that skip the queue aren't held to the pending limit.
        assert_eq!(refusal(&limits, &counts(2, 0, 0, 0), false, false), None);
        assert!(
            refusal(&limits, &counts(0, 5, 0, 0), false, false)
                .unwrap()
                .contains("a day")
        );
        assert_eq!(
            refusal(&UploadLimits::default(), &counts(99, 99, 0, 0), false, true),
            None
        );
    }
}
