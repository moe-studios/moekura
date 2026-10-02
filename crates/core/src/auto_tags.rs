//! Tags a post gets from its file and source, as Danbooru adds them
//! (`Post#add_automatic_tags`): its resolution and shape, what kind of
//! media it is, what its metadata says, and whether its source is a good
//! link. They're worked out again on every upload and edit, so the ones
//! that follow from the file can't be added or removed by hand.
//!
//! Sites name tags their own way, so each rule's tag is a site setting
//! ([`AutomaticTags`]), and the rules apply only when the site turns them
//! on.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::file_traits::FileTrait;
use crate::sites;
use crate::tags::TagName;

/// A post with this many tags no longer needs `tagme`.
pub const TAGME_ENOUGH: usize = 30;

/// One automatic tag's rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// At most 500×500.
    Lowres,
    /// At least 1600 wide or 1200 high.
    Highres,
    /// At least 3200 wide or 2400 high.
    Absurdres,
    /// At least 10000 either way.
    IncrediblyAbsurdres,
    /// At least 1024 wide and four times as wide as high.
    WideImage,
    /// At least 1024 high and four times as high as wide.
    TallImage,
    /// Moves: an animation, video or ugoira.
    Animated,
    AnimatedGif,
    AnimatedPng,
    Video,
    Ugoira,
    /// Has an audio track.
    Sound,
    Greyscale,
    /// Turned by its EXIF orientation.
    ExifRotation,
    /// An animation that plays a few times, then stops.
    NonRepeatingAnimation,
    /// Made by an image generator, as its metadata says.
    AiGenerated,
    /// The source isn't a web address.
    NonWebSource,
    /// The source is an image on a known site that can't lead back to
    /// its page.
    BadLink,
    /// The source is a link on a known site that's neither a work's page
    /// nor an image (a profile, say).
    BadSource,
    /// The post has no tags; taken off again at [`TAGME_ENOUGH`].
    Tagme,
}

/// How a rule's tag comes and goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Follows the file: added when it applies, removed when it doesn't,
    /// whatever was typed.
    File,
    /// Added when the file says so, but may also be added by hand (an
    /// image that looks greyscale but isn't stored so).
    AddOnly,
    /// Follows the source, when that can be told.
    Source,
    /// Follows how many tags the post has.
    Count,
}

impl Rule {
    pub const ALL: [Rule; 20] = [
        Rule::Lowres,
        Rule::Highres,
        Rule::Absurdres,
        Rule::IncrediblyAbsurdres,
        Rule::WideImage,
        Rule::TallImage,
        Rule::Animated,
        Rule::AnimatedGif,
        Rule::AnimatedPng,
        Rule::Video,
        Rule::Ugoira,
        Rule::Sound,
        Rule::Greyscale,
        Rule::ExifRotation,
        Rule::NonRepeatingAnimation,
        Rule::AiGenerated,
        Rule::NonWebSource,
        Rule::BadLink,
        Rule::BadSource,
        Rule::Tagme,
    ];

    /// Its name in settings: Danbooru's tag.
    pub fn key(self) -> &'static str {
        match self {
            Rule::Lowres => "lowres",
            Rule::Highres => "highres",
            Rule::Absurdres => "absurdres",
            Rule::IncrediblyAbsurdres => "incredibly_absurdres",
            Rule::WideImage => "wide_image",
            Rule::TallImage => "tall_image",
            Rule::Animated => "animated",
            Rule::AnimatedGif => "animated_gif",
            Rule::AnimatedPng => "animated_png",
            Rule::Video => "video",
            Rule::Ugoira => "ugoira",
            Rule::Sound => "sound",
            Rule::Greyscale => "greyscale",
            Rule::ExifRotation => "exif_rotation",
            Rule::NonRepeatingAnimation => "non-repeating_animation",
            Rule::AiGenerated => "ai-generated",
            Rule::NonWebSource => "non-web_source",
            Rule::BadLink => "bad_link",
            Rule::BadSource => "bad_source",
            Rule::Tagme => "tagme",
        }
    }

    fn kind(self) -> Kind {
        match self {
            Rule::Greyscale | Rule::AiGenerated => Kind::AddOnly,
            Rule::NonWebSource | Rule::BadLink | Rule::BadSource => Kind::Source,
            Rule::Tagme => Kind::Count,
            _ => Kind::File,
        }
    }

    /// The category a new tag of this rule's gets.
    pub fn category(self) -> &'static str {
        match self {
            Rule::WideImage | Rule::TallImage | Rule::Greyscale => "general",
            _ => "meta",
        }
    }
}

/// The site's automatic tags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutomaticTags {
    pub enabled: bool,
    /// Each rule's tag, by [`Rule::key`], where it isn't the key itself;
    /// empty turns the rule off.
    pub tags: BTreeMap<String, String>,
}

impl AutomaticTags {
    /// `rule`'s tag, if the rule is used.
    pub fn tag(&self, rule: Rule) -> Option<&str> {
        let tag = self.tags.get(rule.key()).map_or(rule.key(), String::as_str);
        (!tag.is_empty()).then_some(tag)
    }

    /// Tags for `rules` from a form's fields (`(key, tag)`): the key itself
    /// is stored as nothing, so the default stays the default.
    pub fn tags_from<'a>(
        fields: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> BTreeMap<String, String> {
        fields
            .into_iter()
            .map(|(key, tag)| (key, crate::tags::normalize(tag)))
            .filter(|(key, tag)| tag != key)
            .map(|(key, tag)| (key.to_owned(), tag))
            .collect()
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        for (key, tag) in &self.tags {
            if !Rule::ALL.iter().any(|r| r.key() == key) {
                return Err(format!("there's no automatic tag `{key}`"));
            }
            if !tag.is_empty()
                && let Err(error) = TagName::parse(tag)
            {
                return Err(format!("the tag for {key} {error}"));
            }
        }
        Ok(())
    }
}

/// What a post's file is, for the rules that follow from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFacts<'a> {
    pub width: i32,
    pub height: i32,
    /// `media_assets.media_type`.
    pub media_type: &'a str,
    pub frames: i32,
    pub has_audio: bool,
    pub traits: &'a [FileTrait],
}

impl FileFacts<'_> {
    /// Whether `rule` (one that follows the file) applies.
    fn says(&self, rule: Rule) -> bool {
        let (w, h) = (i64::from(self.width), i64::from(self.height));
        let video = matches!(self.media_type, "mp4" | "webm");
        let ugoira = self.media_type == "ugoira";
        let moves = self.frames > 1 || video || ugoira;
        match rule {
            Rule::Lowres => w <= 500 && h <= 500,
            Rule::Highres => w >= 1600 || h >= 1200,
            Rule::Absurdres => w >= 3200 || h >= 2400,
            Rule::IncrediblyAbsurdres => w >= 10_000 || h >= 10_000,
            Rule::WideImage => w >= 1024 && h > 0 && w >= 4 * h,
            Rule::TallImage => h >= 1024 && w > 0 && h >= 4 * w,
            Rule::Animated => moves,
            Rule::AnimatedGif => self.media_type == "gif" && self.frames > 1,
            Rule::AnimatedPng => self.media_type == "png" && self.frames > 1,
            Rule::Video => video,
            Rule::Ugoira => ugoira,
            Rule::Sound => self.has_audio,
            Rule::Greyscale => self.traits.contains(&FileTrait::Greyscale),
            Rule::ExifRotation => self.traits.contains(&FileTrait::Rotated),
            Rule::NonRepeatingAnimation => moves && self.traits.contains(&FileTrait::PlaysOnce),
            Rule::AiGenerated => self.traits.contains(&FileTrait::AiGenerated),
            Rule::NonWebSource | Rule::BadLink | Rule::BadSource | Rule::Tagme => false,
        }
    }
}

/// Whether `rule` (one that follows the source) applies to `source`:
/// `None` when it can't be told (a site we don't know).
fn source_says(rule: Rule, source: &str) -> Option<bool> {
    let source = source.trim();
    if rule == Rule::NonWebSource {
        let web = url::Url::parse(source).is_ok_and(|u| matches!(u.scheme(), "http" | "https"));
        return Some(!source.is_empty() && !web);
    }
    let known = sites::parse(source)?;
    Some(match rule {
        Rule::BadLink => known.is_file && known.page_url.is_none(),
        Rule::BadSource => !known.is_file && known.page_url.is_none(),
        _ => false,
    })
}

/// What the rules change in a post's tags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub add: Vec<(String, Rule)>,
    pub remove: Vec<String>,
}

/// What `settings`' rules change in `tags` (a post's, by name), given its
/// file (`None` when there's none yet) and `source`. `tagme` is left to
/// the request tags when the site uses those (`request_tags`).
pub fn changes(
    settings: &AutomaticTags,
    file: Option<&FileFacts<'_>>,
    source: &str,
    tags: &[String],
    request_tags: bool,
) -> Changes {
    let mut changes = Changes::default();
    if !settings.enabled {
        return changes;
    }
    let has = |tag: &str| tags.iter().any(|t| t == tag);
    for rule in Rule::ALL {
        let Some(tag) = settings.tag(rule) else {
            continue;
        };
        let wanted = match rule.kind() {
            Kind::File => file.map(|f| f.says(rule)),
            Kind::AddOnly => file.filter(|f| f.says(rule)).map(|_| true),
            Kind::Source => source_says(rule, source),
            Kind::Count if request_tags => None,
            Kind::Count => {
                // Only the tags typed count, not those added here.
                let typed = tags
                    .iter()
                    .filter(|t| {
                        !Rule::ALL
                            .iter()
                            .any(|r| settings.tag(*r) == Some(t.as_str()))
                    })
                    .count();
                if typed == 0 {
                    Some(true)
                } else if typed >= TAGME_ENOUGH {
                    Some(false)
                } else {
                    None
                }
            }
        };
        match wanted {
            Some(true) if !has(tag) => changes.add.push((tag.to_owned(), rule)),
            Some(false) if has(tag) => changes.remove.push(tag.to_owned()),
            _ => {}
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> AutomaticTags {
        AutomaticTags {
            enabled: true,
            tags: BTreeMap::new(),
        }
    }

    fn png(width: i32, height: i32) -> FileFacts<'static> {
        FileFacts {
            width,
            height,
            media_type: "png",
            frames: 1,
            has_audio: false,
            traits: &[],
        }
    }

    fn names(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|t| (*t).to_owned()).collect()
    }

    fn added(changes: &Changes) -> Vec<&str> {
        changes.add.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn resolution_and_shape() {
        let tags = names(&["cat"]);
        let source = "https://www.pixiv.net/artworks/1";
        let small = changes(&on(), Some(&png(400, 300)), source, &tags, false);
        assert_eq!(added(&small), ["lowres"]);
        let big = changes(&on(), Some(&png(3500, 2000)), source, &tags, false);
        assert_eq!(added(&big), ["highres", "absurdres"]);
        let huge = changes(&on(), Some(&png(10_000, 2000)), source, &tags, false);
        assert_eq!(
            added(&huge),
            ["highres", "absurdres", "incredibly_absurdres", "wide_image"]
        );
        let tall = changes(&on(), Some(&png(300, 1300)), source, &tags, false);
        assert_eq!(added(&tall), ["highres", "tall_image"]);
        // Typed by hand where they don't apply, they go.
        let typed = names(&["cat", "highres", "lowres"]);
        let fixed = changes(&on(), Some(&png(400, 300)), source, &typed, false);
        assert_eq!(fixed.add, []);
        assert_eq!(fixed.remove, ["highres"]);
    }

    #[test]
    fn media_and_metadata() {
        let tags = names(&["cat"]);
        let gif = FileFacts {
            media_type: "gif",
            frames: 12,
            traits: &[FileTrait::PlaysOnce, FileTrait::Greyscale],
            ..png(800, 600)
        };
        assert_eq!(
            added(&changes(&on(), Some(&gif), "", &tags, false)),
            [
                "animated",
                "animated_gif",
                "greyscale",
                "non-repeating_animation"
            ]
        );
        let video = FileFacts {
            media_type: "mp4",
            has_audio: true,
            ..png(800, 600)
        };
        assert_eq!(
            added(&changes(&on(), Some(&video), "", &tags, false)),
            ["animated", "video", "sound"]
        );
        let generated = FileFacts {
            traits: &[FileTrait::AiGenerated, FileTrait::Rotated],
            ..png(800, 600)
        };
        assert_eq!(
            added(&changes(&on(), Some(&generated), "", &tags, false)),
            ["exif_rotation", "ai-generated"]
        );
        // Those that may be tagged by hand stay.
        let typed = names(&["cat", "greyscale", "ai-generated", "animated"]);
        let still = changes(&on(), Some(&png(800, 600)), "", &typed, false);
        assert_eq!(still.remove, ["animated"]);
        // Without a file, nothing about it changes.
        assert_eq!(changes(&on(), None, "", &typed, false), Changes::default());
    }

    #[test]
    fn sources() {
        let tags = names(&["cat"]);
        let file = png(800, 600);
        let add = |source: &str| {
            changes(&on(), Some(&file), source, &tags, false)
                .add
                .into_iter()
                .map(|(t, _)| t)
                .collect::<Vec<_>>()
        };
        assert_eq!(add("my scanner"), ["non-web_source"]);
        assert_eq!(
            add("https://pbs.twimg.com/media/EBGbJe_U8AA4Ekb.jpg"),
            ["bad_link"]
        );
        assert_eq!(add("https://www.pixiv.net/users/5"), ["bad_source"]);
        assert!(add("https://www.pixiv.net/artworks/1").is_empty());
        assert!(add("").is_empty());
        // A good link takes them off; an unknown site leaves them.
        let typed = names(&["cat", "bad_link", "non-web_source"]);
        let fixed = changes(
            &on(),
            Some(&file),
            "https://x.com/a/status/1",
            &typed,
            false,
        );
        assert_eq!(fixed.remove, ["non-web_source", "bad_link"]);
        let unknown = changes(&on(), Some(&file), "https://example.com/a", &typed, false);
        assert_eq!(unknown.remove, ["non-web_source"]);
    }

    #[test]
    fn tagme_and_settings() {
        let file = png(800, 600);
        let none = changes(&on(), Some(&file), "", &[], false);
        assert_eq!(added(&none), ["tagme"]);
        // Left to the request tags when the site uses them.
        assert!(changes(&on(), Some(&file), "", &[], true).add.is_empty());
        let many: Vec<String> = (0..TAGME_ENOUGH)
            .map(|n| format!("t{n}"))
            .chain(["tagme".to_owned()])
            .collect();
        assert_eq!(
            changes(&on(), Some(&file), "", &many, false).remove,
            ["tagme"]
        );

        // Off, renamed, or not used.
        let tags = names(&["cat"]);
        let small = png(300, 200);
        assert_eq!(
            changes(&AutomaticTags::default(), Some(&small), "", &tags, false),
            Changes::default()
        );
        let renamed = AutomaticTags {
            enabled: true,
            tags: [("lowres".to_owned(), "low_resolution".to_owned())].into(),
        };
        assert_eq!(
            added(&changes(&renamed, Some(&small), "", &tags, false)),
            ["low_resolution"]
        );
        let unused = AutomaticTags {
            enabled: true,
            tags: [("lowres".to_owned(), String::new())].into(),
        };
        assert!(
            changes(&unused, Some(&small), "", &tags, false)
                .add
                .is_empty()
        );
    }

    #[test]
    fn settings_from_a_form() {
        let tags =
            AutomaticTags::tags_from([("lowres", "Lowres"), ("highres", "big"), ("video", "")]);
        assert_eq!(
            tags,
            [
                ("highres".to_owned(), "big".to_owned()),
                ("video".to_owned(), String::new())
            ]
            .into()
        );
        let settings = AutomaticTags {
            enabled: true,
            tags,
        };
        assert!(settings.validate().is_ok());
        let bad = AutomaticTags {
            enabled: true,
            tags: [("lowres".to_owned(), "-x".to_owned())].into(),
        };
        assert!(bad.validate().is_err());
        let unknown = AutomaticTags {
            enabled: true,
            tags: [("shiny".to_owned(), "x".to_owned())].into(),
        };
        assert!(unknown.validate().is_err());
    }
}
