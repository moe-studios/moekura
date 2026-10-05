//! The tag box of the post, upload and mass edit forms. Besides tags (with
//! category prefixes) it takes `-tag` to take a tag off, and Danbooru's
//! metatags that change more than tags: `rating:e`, `parent:123`,
//! `pool:name`, `fav`, … ([`Metatag`]).

use std::collections::HashSet;
use std::fmt;

use crate::posts::Rating;
use crate::search::PoolRef;
use crate::tags::{InvalidTag, TagInput, TagName, parse_input};

/// Metatag names the tag box knows, for autocomplete. `-` versions of
/// `parent`, `child`, `pool`, `fav` and `favgroup` undo them.
pub const METATAGS: &[&str] = &[
    "rating", "parent", "child", "source", "pool", "newpool", "fav", "favgroup", "upvote",
    "downvote",
];

/// A change to something other than the post's tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Metatag {
    Rating(Rating),
    /// `parent:123`; `parent:none` and `-parent` for none.
    Parent(Option<i64>),
    /// `-parent:123`: no parent, if it's that one.
    RemoveParent(i64),
    /// `child:123`: makes post 123 a child of this one.
    Child(i64),
    /// `-child:123`: post 123 stops being a child of this one.
    RemoveChild(i64),
    /// `source:url`; `source:none` for none (empty).
    Source(String),
    /// `pool:12` or `pool:name`: adds the post to the end of the pool.
    Pool(PoolRef),
    /// `-pool:12`: takes the post out of the pool.
    RemovePool(PoolRef),
    /// `newpool:name`: starts a pool with the post (or adds it to the pool
    /// of that name, if there is one).
    NewPool(String),
    /// `fav` (true) or `-fav` (false).
    Favorite(bool),
    /// `favgroup:12` or `favgroup:name` (one of the editor's groups);
    /// false for `-favgroup:…`.
    FavoriteGroup(PoolRef, bool),
    /// `upvote` (1) or `downvote` (-1).
    Vote(i16),
}

impl Metatag {
    /// Whether it changes the post itself (and so its history), rather
    /// than something about it.
    pub fn changes_post(&self) -> bool {
        matches!(
            self,
            Metatag::Rating(_) | Metatag::Parent(_) | Metatag::RemoveParent(_) | Metatag::Source(_)
        )
    }
}

/// A metatag that doesn't make sense.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadMetatag {
    pub input: String,
    pub reason: &'static str,
}

impl fmt::Display for BadMetatag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` {}", self.input, self.reason)
    }
}

/// A parsed tag box.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditInput {
    /// Tags to have, first category given winning.
    pub tags: Vec<TagInput>,
    /// Tags to take off (`-tag`); also left out of `tags`.
    pub removed: Vec<TagName>,
    /// In the order typed.
    pub metatags: Vec<Metatag>,
    pub invalid: Vec<InvalidTag>,
    pub bad_metatags: Vec<BadMetatag>,
}

fn post_id(value: &str) -> Option<i64> {
    value
        .trim_start_matches('#')
        .parse()
        .ok()
        .filter(|&id: &i64| id > 0)
}

fn pool_ref(value: &str) -> PoolRef {
    match value.trim_start_matches('#').parse() {
        Ok(id) => PoolRef::Id(id),
        Err(_) => PoolRef::Name(value.to_owned()),
    }
}

/// Reads one word as a metatag: `None` if it isn't one, or the metatag
/// or why it's wrong.
fn metatag(word: &str) -> Option<Result<Metatag, &'static str>> {
    let (negated, rest) = match word.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, word),
    };
    let (name, value) = match rest.split_once(':') {
        Some((name, value)) => (name.to_ascii_lowercase(), Some(value)),
        None => (rest.to_ascii_lowercase(), None),
    };
    let needs = |reason| Err(reason);
    let parsed = match (name.as_str(), negated, value) {
        // Bare words only where they can't be a tag's (`-x` never is),
        // and for Danbooru's `fav`, `upvote` and `downvote`.
        ("parent", true, None) => Ok(Metatag::Parent(None)),
        ("fav", negated, _) => Ok(Metatag::Favorite(!negated)),
        ("upvote", false, _) => Ok(Metatag::Vote(1)),
        ("downvote", false, _) => Ok(Metatag::Vote(-1)),
        (_, _, None) => return None,
        (_, _, Some("")) if METATAGS.contains(&name.as_str()) => needs("needs a value"),
        ("rating", false, Some(value)) => value
            .parse()
            .map(Metatag::Rating)
            .or(needs("isn't a rating: use g, s, q or e")),
        ("parent", false, Some(value)) if value.eq_ignore_ascii_case("none") => {
            Ok(Metatag::Parent(None))
        }
        ("parent", false, Some(value)) => post_id(value)
            .map(|id| Metatag::Parent(Some(id)))
            .ok_or("needs a post number or `none`"),
        ("parent", true, Some(value)) => post_id(value)
            .map(Metatag::RemoveParent)
            .ok_or("needs a post number"),
        ("child", negated, Some(value)) => post_id(value)
            .map(|id| {
                if negated {
                    Metatag::RemoveChild(id)
                } else {
                    Metatag::Child(id)
                }
            })
            .ok_or("needs a post number"),
        ("source", false, Some(value)) if value.eq_ignore_ascii_case("none") => {
            Ok(Metatag::Source(String::new()))
        }
        ("source", false, Some(value)) => Ok(Metatag::Source(value.to_owned())),
        ("pool", false, Some(value)) => Ok(Metatag::Pool(pool_ref(value))),
        ("pool", true, Some(value)) => Ok(Metatag::RemovePool(pool_ref(value))),
        ("newpool", false, Some(value)) => Ok(Metatag::NewPool(value.to_owned())),
        ("favgroup", negated, Some(value)) => Ok(Metatag::FavoriteGroup(pool_ref(value), !negated)),
        (name, true, Some(_)) if METATAGS.contains(&name) => needs("can't be negated"),
        _ => return None,
    };
    Some(parsed)
}

/// Parses a tag box. `categories` are the category prefixes (lower-case
/// category names), as for [`parse_input`]; words in `tags` are tags
/// even if they look like metatags (the box's tags as it was shown, which
/// may predate a metatag).
pub fn parse(input: &str, categories: &[&str], tags: &[&str]) -> EditInput {
    let shown: HashSet<&str> = tags.iter().copied().collect();
    let mut removed: HashSet<TagName> = HashSet::new();
    let mut out = EditInput::default();
    let mut plain = Vec::new();
    for word in input.split_whitespace() {
        if shown.contains(word) {
            plain.push(word);
            continue;
        }
        match metatag(word) {
            Some(Ok(metatag)) => out.metatags.push(metatag),
            Some(Err(reason)) => out.bad_metatags.push(BadMetatag {
                input: word.to_owned(),
                reason,
            }),
            None => match word.strip_prefix('-').filter(|rest| !rest.is_empty()) {
                Some(rest) => match TagName::parse(rest) {
                    Ok(name) => {
                        if removed.insert(name.clone()) {
                            out.removed.push(name);
                        }
                    }
                    Err(error) => out.invalid.push(InvalidTag {
                        input: word.to_owned(),
                        error,
                    }),
                },
                None => plain.push(word),
            },
        }
    }
    let (tags, invalid) = parse_input(&plain.join(" "), categories);
    out.tags = tags
        .into_iter()
        .filter(|t| !removed.contains(&t.name))
        .collect();
    out.invalid.extend(invalid);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metatags(input: &str) -> Vec<Metatag> {
        let parsed = parse(input, &["artist"], &[]);
        assert!(parsed.bad_metatags.is_empty(), "{:?}", parsed.bad_metatags);
        parsed.metatags
    }

    #[test]
    fn reads_metatags() {
        assert_eq!(
            metatags("rating:E parent:#12 -parent child:3 -child:4 source:https://a.example/B"),
            [
                Metatag::Rating(Rating::Explicit),
                Metatag::Parent(Some(12)),
                Metatag::Parent(None),
                Metatag::Child(3),
                Metatag::RemoveChild(4),
                Metatag::Source("https://a.example/B".into()),
            ]
        );
        assert_eq!(
            metatags("parent:none -parent:5 source:none pool:7 pool:My_Comic -pool:7 newpool:New"),
            [
                Metatag::Parent(None),
                Metatag::RemoveParent(5),
                Metatag::Source(String::new()),
                Metatag::Pool(PoolRef::Id(7)),
                Metatag::Pool(PoolRef::Name("My_Comic".into())),
                Metatag::RemovePool(PoolRef::Id(7)),
                Metatag::NewPool("New".into()),
            ]
        );
        assert_eq!(
            metatags("fav -fav fav:me favgroup:2 -favgroup:mine upvote downvote:self"),
            [
                Metatag::Favorite(true),
                Metatag::Favorite(false),
                Metatag::Favorite(true),
                Metatag::FavoriteGroup(PoolRef::Id(2), true),
                Metatag::FavoriteGroup(PoolRef::Name("mine".into()), false),
                Metatag::Vote(1),
                Metatag::Vote(-1),
            ]
        );
    }

    #[test]
    fn keeps_tags_and_removals_apart() {
        let parsed = parse("cat -dog artist:someone -cat -dog -", &["artist"], &[]);
        let tags: Vec<&str> = parsed.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(tags, ["someone"]);
        let removed: Vec<&str> = parsed.removed.iter().map(TagName::as_str).collect();
        assert_eq!(removed, ["dog", "cat"]);
        assert_eq!(parsed.invalid.len(), 1, "a lone `-`");
        assert!(parsed.metatags.is_empty());
    }

    #[test]
    fn refuses_bad_metatags() {
        let parsed = parse(
            "rating:x parent:abc -rating:e pool: -source:x re:zero",
            &[],
            &[],
        );
        let bad: Vec<String> = parsed
            .bad_metatags
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            bad,
            [
                "`rating:x` isn't a rating: use g, s, q or e",
                "`parent:abc` needs a post number or `none`",
                "`-rating:e` can't be negated",
                "`pool:` needs a value",
                "`-source:x` can't be negated",
            ]
        );
        // Not metatags: ordinary tags, or reserved prefixes as before.
        assert_eq!(parsed.tags.len(), 1);
        let (_, invalid) = parse_input("order:score", &[]);
        assert_eq!(parse("order:score", &[], &[]).invalid, invalid);
        // Words shown as tags stay tags.
        let kept = parse("fav cat", &[], &["fav"]);
        assert!(kept.metatags.is_empty());
        assert_eq!(kept.tags.len(), 2);
    }

    #[test]
    fn long_boxes() {
        let shown: Vec<String> = (0..10_000).map(|i| format!("fav_{i}")).collect();
        let shown: Vec<&str> = shown.iter().map(String::as_str).collect();
        let input: String = (0..20_000)
            .map(|i| format!("t{i} -r{i} {} artist:t{i} -t{} ", shown[i % 10_000], i * 2))
            .collect();
        let parsed = parse(&input, &["artist"], &shown);
        // Every odd `t`, and each shown word once.
        assert_eq!(parsed.tags.len(), 10_000 + 10_000);
        assert_eq!(parsed.tags[0].name.as_str(), "fav_0");
        assert_eq!(parsed.tags[1].name.as_str(), "t1");
        assert_eq!(parsed.tags[1].category.as_deref(), Some("artist"));
        assert_eq!(parsed.removed.len(), 20_000 + 20_000);
        assert!(parsed.invalid.is_empty() && parsed.metatags.is_empty());
    }
}
