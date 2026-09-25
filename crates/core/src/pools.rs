//! Pool names and categories.

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

/// Post ids typed as text: separated by whitespace or commas, `#` allowed
/// in front, duplicates dropped (the first stays). Returns the offending
/// word if one isn't an id.
pub fn parse_post_ids(text: &str) -> Result<Vec<i64>, String> {
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
            .ok_or_else(|| word.to_owned())?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
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
        assert_eq!(parse_post_ids("1 x 2"), Err("x".into()));
        assert_eq!(parse_post_ids("0"), Err("0".into()));
    }
}
