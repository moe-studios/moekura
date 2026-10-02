//! Tag names: how raw input is normalised and which names are allowed.

use std::fmt;

/// Longest tag name, in characters.
pub const TAG_MAX_LEN: usize = 170;
/// Most tags one post may carry.
pub const POST_MAX_TAGS: usize = 1000;

/// Words that can't begin a tag name when followed by `:`, because search
/// reads `word:value` as a metatag or a tag category prefix.
///
/// Includes metatags planned for later milestones, so no tag can claim
/// them in the meantime. Adding a word here later needs a migration that
/// renames existing tags starting with it.
pub const RESERVED_PREFIXES: &[&str] = &[
    // Metatags.
    "ai",
    "approver",
    "arttags",
    "chartags",
    "child",
    "comment",
    "commentary",
    "commentcount",
    "commenter",
    "copytags",
    "date",
    "downvote",
    "embedded",
    "exif",
    "duration",
    "fav",
    "favgroup",
    "favcount",
    "filesize",
    "filetype",
    "flagger",
    "gentags",
    "has",
    "height",
    "id",
    "is",
    "limit",
    "md5",
    "metatags",
    "mpixels",
    "newpool",
    "note",
    "notecount",
    "noter",
    "order",
    "ordfav",
    "ordfavgroup",
    "ordpool",
    "parent",
    "pixiv",
    "pixiv_id",
    "pool",
    "rating",
    "ratio",
    "score",
    "similar",
    "search",
    "source",
    "status",
    "tagcount",
    "upvote",
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

/// A tag from a tag input box: `name`, or `category:name` to choose the
/// category of a new tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagInput {
    pub name: TagName,
    pub category: Option<String>,
}

/// A word from a tag input box that isn't a valid tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTag {
    pub input: String,
    pub error: TagNameError,
}

impl fmt::Display for InvalidTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` {}", self.input, self.error)
    }
}

/// Splits whitespace-separated tags, recognising the prefixes in
/// `categories` (lower-case category names). Duplicates are dropped; the
/// first category given for a tag wins.
pub fn parse_input(input: &str, categories: &[&str]) -> (Vec<TagInput>, Vec<InvalidTag>) {
    let mut tags: Vec<TagInput> = Vec::new();
    let mut invalid = Vec::new();
    for word in input.split_whitespace() {
        let (category, raw) = match word.split_once(':') {
            Some((prefix, rest))
                if !rest.is_empty() && categories.contains(&prefix.to_lowercase().as_str()) =>
            {
                (Some(prefix.to_lowercase()), rest)
            }
            _ => (None, word),
        };
        match TagName::parse(raw) {
            Ok(name) => match tags.iter_mut().find(|t| t.name == name) {
                Some(existing) => {
                    existing.category = existing.category.take().or(category);
                }
                None => tags.push(TagInput { name, category }),
            },
            Err(error) => invalid.push(InvalidTag {
                input: word.to_owned(),
                error,
            }),
        }
    }
    (tags, invalid)
}

/// Danbooru's tag categories, by id. The Danbooru-compatible API passes
/// ids through and imports map these names, so they keep both; their
/// labels and order can change.
pub const BUILT_IN_CATEGORIES: &[(i16, &str)] = &[
    (0, "general"),
    (1, "artist"),
    (3, "copyright"),
    (4, "character"),
    (5, "meta"),
];

/// Categories a site adds get ids from here up, past Danbooru's.
pub const FIRST_CUSTOM_CATEGORY: i16 = 6;

/// The longest category name, which is also its prefix in tag boxes.
pub const CATEGORY_NAME_MAX_LEN: usize = 32;
/// The longest category label, in characters.
pub const CATEGORY_LABEL_MAX_LEN: usize = 64;

/// Whether category `id` is one of Danbooru's.
pub fn is_built_in_category(id: i16) -> bool {
    BUILT_IN_CATEGORIES
        .iter()
        .any(|(built_in, _)| *built_in == id)
}

/// Why `name` can't name a new category: it must be a lowercase word
/// that search and tag boxes don't already read as something else.
pub fn check_category_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let word = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.len() <= CATEGORY_NAME_MAX_LEN;
    if !word {
        return Err(format!(
            "A category's name is up to {CATEGORY_NAME_MAX_LEN} lowercase letters, digits \
             and underscores, starting with a letter"
        ));
    }
    let short = crate::search::CATEGORY_SHORT_NAMES
        .iter()
        .any(|(short, _)| *short == name);
    if RESERVED_PREFIXES.contains(&name) || short {
        return Err(format!("`{name}` already means something in searches"));
    }
    Ok(())
}

/// Why `label` can't label a category.
pub fn check_category_label(label: &str) -> Result<(), String> {
    let length = label.chars().count();
    if length == 0 || length > CATEGORY_LABEL_MAX_LEN || label.chars().any(char::is_control) {
        return Err(format!(
            "A category's label is 1 to {CATEGORY_LABEL_MAX_LEN} characters"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_category_names() {
        assert!(check_category_name("species").is_ok());
        assert!(check_category_name("lore_2").is_ok());
        for bad in [
            "",
            "Species",
            "2d",
            "has space",
            "rating",
            "artist",
            "gen",
            &"x".repeat(33),
        ] {
            assert!(check_category_name(bad).is_err(), "{bad}");
        }
        assert!(check_category_label("Species").is_ok());
        assert!(check_category_label("").is_err());
        assert!(is_built_in_category(3) && !is_built_in_category(6));
    }

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

    #[test]
    fn parses_tag_input() {
        let categories = ["artist", "character"];
        let (tags, invalid) = parse_input(
            "1girl  Artist:Some_One solo\ncharacter:saber_(fate) 1girl artist:solo re:zero -x",
            &categories,
        );
        let summary: Vec<(&str, Option<&str>)> = tags
            .iter()
            .map(|t| (t.name.as_str(), t.category.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("1girl", None),
                ("some_one", Some("artist")),
                ("solo", Some("artist")),
                ("saber_(fate)", Some("character")),
                ("re:zero", None),
            ]
        );
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid[0].to_string(), "`-x` may not start with `-`");
        // A bare prefix is a (reserved) tag, not a category.
        let (tags, invalid) = parse_input("artist:", &categories);
        assert!(tags.is_empty());
        assert_eq!(
            invalid[0].error,
            TagNameError::ReservedPrefix("artist".into())
        );
    }
}
