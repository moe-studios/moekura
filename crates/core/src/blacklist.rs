//! Blacklists: posts a viewer doesn't want to see.
//!
//! One rule per line. A rule is space-separated terms that must all hold:
//! `tag` (has it), `-tag` (hasn't), `rating:e,q` (has one of these
//! ratings) or `-rating:g`. A post is blacklisted when any rule matches.

use std::fmt;

use crate::posts::Rating;
use crate::tags::{TagName, TagNameError};

/// Most rules, and characters, a blacklist may have.
pub const MAX_RULES: usize = 200;
pub const MAX_LEN: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rule {
    pub include: Vec<TagName>,
    pub exclude: Vec<TagName>,
    /// Each list is one `rating:` term; the post's rating must be in it.
    pub ratings: Vec<Vec<Rating>>,
    /// The post's rating must not be in any of these.
    pub not_ratings: Vec<Rating>,
    /// The line as written, to show which rule matched.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Blacklist {
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BlacklistError {
    #[error("`{term}`: the tag {error}")]
    Tag { term: String, error: TagNameError },
    #[error("`{0}`: expected ratings like g, s, q or e")]
    Rating(String),
    #[error("`{0}`: only tags, -tags and rating: work in a blacklist")]
    Unsupported(String),
    #[error("A blacklist may have at most {MAX_RULES} rules and {MAX_LEN} characters.")]
    TooLong,
}

impl Blacklist {
    pub fn parse(text: &str) -> Result<Self, BlacklistError> {
        if text.len() > MAX_LEN {
            return Err(BlacklistError::TooLong);
        }
        let mut rules = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let mut rule = Rule {
                text: line.to_owned(),
                ..Rule::default()
            };
            for term in line.split_whitespace() {
                let (negated, rest) = match term.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, term),
                };
                if let Some(value) = rest.strip_prefix("rating:") {
                    let ratings: Option<Vec<Rating>> =
                        value.split(',').map(|r| r.parse().ok()).collect();
                    let ratings = ratings
                        .filter(|r| !r.is_empty())
                        .ok_or_else(|| BlacklistError::Rating(term.into()))?;
                    if negated {
                        rule.not_ratings.extend(ratings);
                    } else {
                        rule.ratings.push(ratings);
                    }
                    continue;
                }
                if rest.contains(':') && rest.split(':').next().is_some_and(is_metatag) {
                    return Err(BlacklistError::Unsupported(term.into()));
                }
                let name = TagName::parse(rest).map_err(|error| BlacklistError::Tag {
                    term: term.into(),
                    error,
                })?;
                if negated {
                    rule.exclude.push(name);
                } else {
                    rule.include.push(name);
                }
            }
            rules.push(rule);
        }
        if rules.len() > MAX_RULES {
            return Err(BlacklistError::TooLong);
        }
        Ok(Self { rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Every tag the rules mention, for resolving them to ids.
    pub fn tag_names(&self) -> impl Iterator<Item = &TagName> {
        self.rules
            .iter()
            .flat_map(|r| r.include.iter().chain(&r.exclude))
    }

    /// The first rule matching a post with these tags and rating. `has`
    /// answers whether the post has a tag.
    pub fn matching(&self, rating: Rating, has: impl Fn(&TagName) -> bool) -> Option<&Rule> {
        self.rules.iter().find(|rule| {
            rule.include.iter().all(&has)
                && !rule.exclude.iter().any(&has)
                && rule.ratings.iter().all(|allowed| allowed.contains(&rating))
                && !rule.not_ratings.contains(&rating)
        })
    }
}

fn is_metatag(prefix: &str) -> bool {
    crate::search::METATAGS.contains(&prefix.to_lowercase().as_str())
}

impl fmt::Display for Blacklist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for rule in &self.rules {
            writeln!(f, "{}", rule.text)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(names: &[&str]) -> impl Fn(&TagName) -> bool {
        let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        move |tag| names.iter().any(|n| n == tag.as_str())
    }

    #[test]
    fn rules_match_all_their_terms() {
        let list =
            Blacklist::parse("  spiders \n\ngore -safe_version\nrating:e cat\n-rating:g,s dog")
                .unwrap();
        assert_eq!(list.rules.len(), 4);
        let rule = |rating, names| list.matching(rating, tags(names)).map(|r| r.text.as_str());
        assert_eq!(rule(Rating::General, &["spiders", "cat"]), Some("spiders"));
        assert_eq!(rule(Rating::General, &["gore"]), Some("gore -safe_version"));
        assert_eq!(rule(Rating::General, &["gore", "safe_version"]), None);
        assert_eq!(rule(Rating::Explicit, &["cat"]), Some("rating:e cat"));
        assert_eq!(rule(Rating::Questionable, &["cat"]), None);
        assert_eq!(
            rule(Rating::Questionable, &["dog"]),
            Some("-rating:g,s dog")
        );
        assert_eq!(rule(Rating::Sensitive, &["dog"]), None);
        // A rule of only a rating hides everything with it.
        let explicit = Blacklist::parse("rating:e").unwrap();
        assert!(explicit.matching(Rating::Explicit, tags(&[])).is_some());
        assert!(Blacklist::parse("").unwrap().is_empty());
    }

    #[test]
    fn refuses_what_it_cant_evaluate() {
        assert_eq!(
            Blacklist::parse("score:<0").unwrap_err().to_string(),
            "`score:<0`: only tags, -tags and rating: work in a blacklist"
        );
        assert!(matches!(
            Blacklist::parse("rating:x"),
            Err(BlacklistError::Rating(_))
        ));
        assert!(matches!(
            Blacklist::parse("long*"),
            Err(BlacklistError::Tag { .. })
        ));
        assert_eq!(
            Blacklist::parse(&"x\n".repeat(MAX_RULES + 1)),
            Err(BlacklistError::TooLong)
        );
        // Colons in ordinary tags are fine.
        assert!(Blacklist::parse("re:zero").is_ok());
    }
}
