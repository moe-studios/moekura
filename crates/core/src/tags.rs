//! Tag names: how raw input is normalised and which names are allowed.

use std::fmt;

/// Longest tag name, in characters.
pub const TAG_MAX_LEN: usize = 170;

/// Words that can't begin a tag name when followed by `:`, because search
/// reads `word:value` as a metatag or a tag category prefix.
///
/// Includes metatags planned for later milestones, so no tag can claim
/// them in the meantime. Adding a word here later needs a migration that
/// renames existing tags starting with it.
pub const RESERVED_PREFIXES: &[&str] = &[
    // Metatags.
    "approver",
    "comment",
    "commenter",
    "date",
    "duration",
    "fav",
    "favcount",
    "filesize",
    "filetype",
    "height",
    "id",
    "limit",
    "md5",
    "mpixels",
    "note",
    "order",
    "parent",
    "pool",
    "rating",
    "ratio",
    "score",
    "similar",
    "source",
    "status",
    "tagcount",
    "user",
    "width",
    // Default tag categories.
    "artist",
    "character",
    "copyright",
    "general",
    "meta",
];

/// A valid, normalised tag name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TagName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TagNameError {
    #[error("is empty")]
    Empty,
    #[error("is longer than {TAG_MAX_LEN} characters")]
    TooLong,
    #[error("may not start with `{0}`")]
    BadStart(char),
    #[error("may not contain `*`")]
    Asterisk,
    #[error("may not contain control characters")]
    ControlCharacter,
    #[error("may not start with `{0}:`")]
    ReservedPrefix(String),
}

/// Lower-cases `raw`, turns runs of whitespace and underscores into a
/// single `_`, and trims underscores from both ends.
pub fn normalize(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars().flat_map(char::to_lowercase) {
        let c = if c.is_whitespace() { '_' } else { c };
        if c == '_' && (out.is_empty() || out.ends_with('_')) {
            continue;
        }
        out.push(c);
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

impl TagName {
    /// Normalises `raw` and checks the result.
    pub fn parse(raw: &str) -> Result<Self, TagNameError> {
        let name = normalize(raw);
        let first = name.chars().next().ok_or(TagNameError::Empty)?;
        if name.chars().count() > TAG_MAX_LEN {
            return Err(TagNameError::TooLong);
        }
        // Search syntax: `-tag` excludes and `~tag` makes an or-group.
        if matches!(first, '-' | '~') {
            return Err(TagNameError::BadStart(first));
        }
        if name.contains('*') {
            return Err(TagNameError::Asterisk);
        }
        if name.chars().any(char::is_control) {
            return Err(TagNameError::ControlCharacter);
        }
        if let Some((prefix, _)) = name.split_once(':')
            && RESERVED_PREFIXES.contains(&prefix)
        {
            return Err(TagNameError::ReservedPrefix(prefix.to_owned()));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for TagName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for TagName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<String, TagNameError> {
        TagName::parse(raw).map(TagName::into_string)
    }

    #[test]
    fn normalizes() {
        assert_eq!(normalize("  Long Hair "), "long_hair");
        assert_eq!(normalize("a \t b__c___"), "a_b_c");
        assert_eq!(normalize("__x"), "x");
        assert_eq!(normalize("ÉCOLE"), "école");
        assert_eq!(normalize("初音ミク"), "初音ミク");
    }

    #[test]
    fn accepts_ordinary_names() {
        for name in [
            "1girl",
            "fate/stay_night",
            "saber_(fate)",
            ":d",
            ":3",
            "^_^",
            "hello,_world",
            "re:zero",
            "c++",
        ] {
            assert_eq!(parse(name).as_deref(), Ok(name), "{name}");
        }
    }

    #[test]
    fn rejects_bad_names() {
        assert_eq!(parse("  "), Err(TagNameError::Empty));
        assert_eq!(parse("___"), Err(TagNameError::Empty));
        assert_eq!(parse("-solo"), Err(TagNameError::BadStart('-')));
        assert_eq!(parse("~solo"), Err(TagNameError::BadStart('~')));
        assert_eq!(parse("long*"), Err(TagNameError::Asterisk));
        assert_eq!(parse("a\u{7}b"), Err(TagNameError::ControlCharacter));
        assert_eq!(
            parse("rating:e"),
            Err(TagNameError::ReservedPrefix("rating".into()))
        );
        assert_eq!(
            parse("Artist:someone"),
            Err(TagNameError::ReservedPrefix("artist".into()))
        );
        assert_eq!(parse(&"a".repeat(TAG_MAX_LEN)).unwrap().len(), TAG_MAX_LEN);
        assert_eq!(
            parse(&"a".repeat(TAG_MAX_LEN + 1)),
            Err(TagNameError::TooLong)
        );
        // Characters, not bytes.
        assert!(parse(&"ミ".repeat(TAG_MAX_LEN)).is_ok());
    }
}
