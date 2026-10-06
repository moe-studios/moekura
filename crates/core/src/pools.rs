//! Pool names and categories.

use std::collections::HashSet;
use std::fmt;

/// Longest pool name, in characters.
pub const NAME_MAX_LEN: usize = 170;

/// Most posts in one pool.
pub const MAX_POSTS: usize = 10_000;

/// Names a pool can't have, since searches give them another meaning.
const RESERVED_NAMES: &[&str] = &["any", "none"];

/// A valid pool name: whitespace runs become a single `_`, case is kept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolNameError {
    #[error("is empty")]
    Empty,
    #[error("is longer than {NAME_MAX_LEN} characters")]
    TooLong,
    #[error("can't be only digits, which would read as a pool id")]
    Numeric,
    #[error("may not contain `{0}`")]
    BadCharacter(char),
    #[error("can't be `{0}`")]
    Reserved(String),
}

impl PoolName {
    pub fn parse(raw: &str) -> Result<Self, PoolNameError> {
        let name = raw
            .split(|c: char| c.is_whitespace() || c == '_')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("_");
        if name.is_empty() {
            return Err(PoolNameError::Empty);
        }
        if name.chars().count() > NAME_MAX_LEN {
            return Err(PoolNameError::TooLong);
        }
        if name.bytes().all(|b| b.is_ascii_digit()) {
            return Err(PoolNameError::Numeric);
        }
        // `*` is a wildcard in name searches, `,` separates lists.
        if let Some(c) = name
            .chars()
            .find(|c| matches!(c, '*' | ',') || c.is_control())
        {
            return Err(PoolNameError::BadCharacter(c));
        }
        if RESERVED_NAMES.contains(&name.to_lowercase().as_str()) {
            return Err(PoolNameError::Reserved(name));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// How the name reads: underscores as spaces.
    pub fn display(name: &str) -> String {
        name.replace('_', " ")
    }
}

impl fmt::Display for PoolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Category {
    /// Posts meant to be read in order: a comic, a series.
    #[default]
    Series,
    /// A loose group of posts.
    Collection,
}

impl Category {
    pub const ALL: [Category; 2] = [Category::Series, Category::Collection];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Series => "series",
            Category::Collection => "collection",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// Why a list of post ids was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PostIdsError {
    /// This word isn't a post id.
    #[error("“{0}” isn't a post number")]
    NotAnId(String),
    /// More than [`MAX_POSTS`] different posts, which no pool or group
    /// may hold.
    #[error("more than {MAX_POSTS} posts")]
    TooMany,
}

/// `ids` in order with duplicates dropped (the first stays). Stops at the
/// first post past [`MAX_POSTS`], so a huge list costs no more than a
/// full one.
pub fn unique_post_ids(ids: impl IntoIterator<Item = i64>) -> Result<Vec<i64>, PostIdsError> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for id in ids {
        if seen.insert(id) {
            if unique.len() == MAX_POSTS {
                return Err(PostIdsError::TooMany);
            }
            unique.push(id);
        }
    }
    Ok(unique)
}

/// Post ids typed as text: separated by whitespace or commas, `#` allowed
/// in front, duplicates dropped (the first stays), at most [`MAX_POSTS`]
/// of them.
pub fn parse_post_ids(text: &str) -> Result<Vec<i64>, PostIdsError> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || c == ',') {
        if word.is_empty() {
            continue;
        }
        let id = word
            .trim_start_matches('#')
            .parse::<i64>()
            .ok()
            .filter(|&id| id > 0)
            .ok_or_else(|| PostIdsError::NotAnId(word.to_owned()))?;
        if seen.insert(id) {
            if ids.len() == MAX_POSTS {
                return Err(PostIdsError::TooMany);
            }
            ids.push(id);
        }
    }
    Ok(ids)
}

/// What one version of a pool's posts changed from the one before: the
/// posts it added (in its order) and those it removed (in `before`'s).
/// Through sets, so a full pool's history costs no more than its length.
pub fn post_changes(before: &[i64], after: &[i64]) -> (Vec<i64>, Vec<i64>) {
    let had: HashSet<i64> = before.iter().copied().collect();
    let has: HashSet<i64> = after.iter().copied().collect();
    let added = after
        .iter()
        .copied()
        .filter(|id| !had.contains(id))
        .collect();
    let removed = before
        .iter()
        .copied()
        .filter(|id| !has.contains(id))
        .collect();
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            PoolName::parse("  My  Comic_ Part 2 ").unwrap().as_str(),
            "My_Comic_Part_2"
        );
        assert_eq!(PoolName::parse(" _ "), Err(PoolNameError::Empty));
        assert_eq!(PoolName::parse("123"), Err(PoolNameError::Numeric));
        assert!(PoolName::parse("123abc").is_ok());
        assert_eq!(
            PoolName::parse("a*b"),
            Err(PoolNameError::BadCharacter('*'))
        );
        assert_eq!(
            PoolName::parse("None"),
            Err(PoolNameError::Reserved("None".into()))
        );
        assert_eq!(
            PoolName::parse(&"x".repeat(171)),
            Err(PoolNameError::TooLong)
        );
        assert_eq!(PoolName::display("My_Comic"), "My Comic");
    }

    #[test]
    fn post_id_lists() {
        assert_eq!(parse_post_ids(" 3, #1\n2 3 "), Ok(vec![3, 1, 2]));
        assert_eq!(parse_post_ids(""), Ok(vec![]));
        assert_eq!(
            parse_post_ids("1 x 2"),
            Err(PostIdsError::NotAnId("x".into()))
        );
        assert_eq!(parse_post_ids("0"), Err(PostIdsError::NotAnId("0".into())));
    }

    #[test]
    fn post_id_lists_are_capped() {
        let full: Vec<String> = (1..=MAX_POSTS).map(|id| id.to_string()).collect();
        // Repeats don't count.
        let text = format!("{} 1 1 {}", full.join(" "), full.join(","));
        assert_eq!(parse_post_ids(&text).map(|ids| ids.len()), Ok(MAX_POSTS));
        let over = format!("{} {}", full.join(" "), MAX_POSTS + 1);
        assert_eq!(parse_post_ids(&over), Err(PostIdsError::TooMany));
        // It stops there: a bad word after the cap isn't reached.
        assert_eq!(
            parse_post_ids(&format!("{over} x")),
            Err(PostIdsError::TooMany)
        );

        assert_eq!(unique_post_ids([3, 1, 3, 2, 1]), Ok(vec![3, 1, 2]));
        let twice = (1..=MAX_POSTS as i64).chain(1..=MAX_POSTS as i64);
        assert_eq!(unique_post_ids(twice).map(|ids| ids.len()), Ok(MAX_POSTS));
        // A huge list is refused as soon as it's too long.
        let over = (1..)
            .take(MAX_POSTS + 1)
            .chain(std::iter::from_fn(|| panic!("read on past the cap")));
        assert_eq!(unique_post_ids(over), Err(PostIdsError::TooMany));
    }

    #[test]
    fn changes_between_versions() {
        assert_eq!(post_changes(&[], &[2, 1]), (vec![2, 1], vec![]));
        assert_eq!(post_changes(&[1, 2, 3], &[4, 3, 1]), (vec![4], vec![2]));
        // Reordering adds and removes nothing.
        assert_eq!(post_changes(&[1, 2], &[2, 1]), (vec![], vec![]));
        let full = MAX_POSTS as i64;
        let before: Vec<i64> = (1..=full).collect();
        let after: Vec<i64> = (full / 2 + 1..=full + full / 2).collect();
        let (added, removed) = post_changes(&before, &after);
        assert_eq!((added.len(), removed.len()), (MAX_POSTS / 2, MAX_POSTS / 2));
        assert_eq!((added[0], removed[0]), (full + 1, 1));
    }
}
