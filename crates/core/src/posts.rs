//! Post vocabulary shared by the database and web layers.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Content rating, stored as a single letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rating {
    #[serde(rename = "g")]
    General,
    #[serde(rename = "s")]
    Sensitive,
    #[serde(rename = "q")]
    Questionable,
    #[serde(rename = "e")]
    Explicit,
}

impl Rating {
    pub const ALL: [Rating; 4] = [
        Rating::General,
        Rating::Sensitive,
        Rating::Questionable,
        Rating::Explicit,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Rating::General => "g",
            Rating::Sensitive => "s",
            Rating::Questionable => "q",
            Rating::Explicit => "e",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Rating::General => "General",
            Rating::Sensitive => "Sensitive",
            Rating::Questionable => "Questionable",
            Rating::Explicit => "Explicit",
        }
    }
}

impl FromStr for Rating {
    type Err = ();

    /// Accepts the letter or the full name, case-insensitively.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        Self::ALL
            .into_iter()
            .find(|r| r.code().eq_ignore_ascii_case(s) || r.label().eq_ignore_ascii_case(s))
            .ok_or(())
    }
}

impl fmt::Display for Rating {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PostStatus {
    /// Waiting in the approval queue; hidden from most users.
    Pending,
    Active,
    /// Reported for review; still visible.
    Flagged,
    /// Soft-deleted; visible only with the ViewDeleted permission.
    Deleted,
}

impl PostStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PostStatus::Pending => "pending",
            PostStatus::Active => "active",
            PostStatus::Flagged => "flagged",
            PostStatus::Deleted => "deleted",
        }
    }
}

impl FromStr for PostStatus {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [Self::Pending, Self::Active, Self::Flagged, Self::Deleted]
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or(())
    }
}

pub const SOURCE_MAX_LEN: usize = 2048;
pub const DESCRIPTION_MAX_LEN: usize = 20000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratings_parse_from_code_or_name() {
        assert_eq!("e".parse(), Ok(Rating::Explicit));
        assert_eq!("Questionable".parse(), Ok(Rating::Questionable));
        assert_eq!(" G ".parse(), Ok(Rating::General));
        assert_eq!("x".parse::<Rating>(), Err(()));
        for r in Rating::ALL {
            assert_eq!(r.code().parse(), Ok(r));
        }
    }

    #[test]
    fn statuses_round_trip() {
        for s in ["pending", "active", "flagged", "deleted"] {
            assert_eq!(s.parse::<PostStatus>().unwrap().as_str(), s);
        }
    }
}
