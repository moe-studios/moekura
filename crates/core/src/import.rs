//! Reading the tag files that sit next to imported files ("sidecars"), in
//! the formats common downloaders and managers write.
//!
//! - `pic.png.txt` or `pic.txt`: tags one per line (gallery-dl, Hydrus;
//!   spaces within a tag become underscores), or on one line separated by
//!   commas or spaces. `namespace:tag` lines keep their namespace, mapped
//!   to a category where one matches (`creator:` is `artist:`, `series:`
//!   is `copyright:`); `rating:` sets the rating.
//! - `pic.png.json` or `pic.json`: an object with `tags` (a list, a
//!   string, or lists by category), or Danbooru's `tag_string` and
//!   `tag_string_<category>`; and optionally `rating`, `source` and
//!   `description`.
//!
//! Tags come out as the upload form takes them: `category:name` or
//! `name`, not yet validated.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::posts::Rating;

/// File extensions imported; everything else in a folder is ignored.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "jxl", "mp4", "webm",
];

/// Whether `path` looks like a file to import, by its extension.
pub fn is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MEDIA_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Where a sidecar for `path` may be, in the order they're looked for:
/// `pic.png.json`, `pic.json`, `pic.png.txt`, `pic.txt`.
pub fn sidecar_paths(path: &Path) -> Vec<PathBuf> {
    let with = |ext: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{ext}"));
        PathBuf::from(name)
    };
    let mut paths = Vec::new();
    for ext in ["json", "txt"] {
        paths.push(with(ext));
        paths.push(path.with_extension(ext));
    }
    paths
}

/// What a sidecar says about its file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sidecar {
    pub tags: Vec<String>,
    pub rating: Option<Rating>,
    pub source: Option<String>,
    pub description: Option<String>,
}

impl Sidecar {
    /// Adds `other`'s tags, and its fields where this has none.
    pub fn merge(&mut self, other: Sidecar) {
        self.tags.extend(other.tags);
        self.rating = self.rating.or(other.rating);
        self.source = self.source.take().or(other.source);
        self.description = self.description.take().or(other.description);
    }
}

/// A rating as other sites write it: our letters and names, plus `safe`
/// (older Danbooru, most boorus) for general.
pub fn parse_rating(text: &str) -> Option<Rating> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("safe") {
        return Some(Rating::General);
    }
    text.parse().ok()
}

/// Namespaces other tools use, and the category each means here.
const NAMESPACES: &[(&str, &str)] = &[
    ("creator", "artist"),
    ("artist", "artist"),
    ("series", "copyright"),
    ("copyright", "copyright"),
    ("character", "character"),
    ("meta", "meta"),
    ("general", "general"),
];

/// One tag as written by another tool, in upload-form form. `None` for a
/// `rating:` tag, which goes to `rating` instead.
fn tag(raw: &str, rating: &mut Option<Rating>) -> Option<String> {
    let raw = raw.trim();
    let joined = |s: &str| s.split_whitespace().collect::<Vec<_>>().join("_");
    if let Some((namespace, rest)) = raw.split_once(':')
        && !rest.trim().is_empty()
    {
        let namespace = namespace.trim().to_lowercase();
        if namespace == "rating" {
            *rating = rating.or(parse_rating(rest));
            return None;
        }
        if let Some((_, category)) = NAMESPACES.iter().find(|(n, _)| *n == namespace) {
            let name = joined(rest);
            return Some(if *category == "general" {
                name
            } else {
                format!("{category}:{name}")
            });
        }
    }
    let name = joined(raw);
    (!name.is_empty()).then_some(name)
}

/// A `.txt` sidecar.
pub fn parse_txt(text: &str) -> Sidecar {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let raw: Vec<&str> = match lines.as_slice() {
        [] => Vec::new(),
        [line] if line.contains(',') => line.split(',').collect(),
        [line] => line.split_whitespace().collect(),
        many => many.to_vec(),
    };
    let mut sidecar = Sidecar::default();
    sidecar.tags = raw
        .into_iter()
        .filter_map(|t| tag(t, &mut sidecar.rating))
        .collect();
    sidecar
}

/// A `.json` sidecar.
pub fn parse_json(text: &str) -> Result<Sidecar, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "expected a JSON object".to_owned())?;
    let mut sidecar = Sidecar::default();
    let mut rating = None;
    let mut add = |category: Option<&str>, raw: &str, rating: &mut Option<Rating>| {
        let Some(tag) = tag(raw, rating) else { return };
        match category {
            Some(category) if category != "general" && !tag.contains(':') => {
                sidecar.tags.push(format!("{category}:{tag}"));
            }
            _ => sidecar.tags.push(tag),
        }
    };
    // A list of tags, or a string of them separated by spaces.
    let each = |value: &Value| -> Vec<String> {
        match value {
            Value::Array(items) => items
                .iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect(),
            Value::String(s) => s.split_whitespace().map(str::to_owned).collect(),
            _ => Vec::new(),
        }
    };
    match object.get("tags") {
        // e621 and others: lists by category.
        Some(Value::Object(groups)) => {
            for (group, list) in groups {
                let category = NAMESPACES.iter().find(|(n, _)| n == group).map(|(_, c)| *c);
                for raw in each(list) {
                    add(category, &raw, &mut rating);
                }
            }
        }
        Some(list) => {
            for raw in each(list) {
                add(None, &raw, &mut rating);
            }
        }
        None => {}
    }
    // Danbooru: tag_string_<category>, or failing that tag_string.
    let mut by_category = false;
    for (key, category) in [
        ("tag_string_general", "general"),
        ("tag_string_artist", "artist"),
        ("tag_string_copyright", "copyright"),
        ("tag_string_character", "character"),
        ("tag_string_meta", "meta"),
    ] {
        if let Some(list) = object.get(key) {
            by_category = true;
            for raw in each(list) {
                add(Some(category), &raw, &mut rating);
            }
        }
    }
    if !by_category && let Some(list) = object.get("tag_string") {
        for raw in each(list) {
            add(None, &raw, &mut rating);
        }
    }
    let text = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    sidecar.rating = text("rating").and_then(|r| parse_rating(&r)).or(rating);
    sidecar.source = text("source");
    sidecar.description = text("description");
    Ok(sidecar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_media_and_sidecars() {
        assert!(is_media(Path::new("a/B.PNG")));
        assert!(!is_media(Path::new("a/b.txt")));
        assert!(!is_media(Path::new("a/noext")));
        assert_eq!(
            sidecar_paths(Path::new("d/pic.png")),
            [
                PathBuf::from("d/pic.png.json"),
                PathBuf::from("d/pic.json"),
                PathBuf::from("d/pic.png.txt"),
                PathBuf::from("d/pic.txt"),
            ]
        );
    }

    #[test]
    fn reads_text_sidecars() {
        // One per line, as gallery-dl and Hydrus write them.
        let hydrus = "long hair\ncreator:some one\nseries:a show\nrating:explicit\nperson:x\n\n";
        assert_eq!(
            parse_txt(hydrus),
            Sidecar {
                tags: vec![
                    "long_hair".into(),
                    "artist:some_one".into(),
                    "copyright:a_show".into(),
                    "person:x".into(),
                ],
                rating: Some(Rating::Explicit),
                ..Sidecar::default()
            }
        );
        assert_eq!(parse_txt("cat dog  ").tags, ["cat", "dog"]);
        assert_eq!(
            parse_txt("cat, long hair,dog").tags,
            ["cat", "long_hair", "dog"]
        );
        assert_eq!(parse_txt("").tags, Vec::<String>::new());
    }

    #[test]
    fn reads_json_sidecars() {
        let plain = r#"{"tags": ["cat", "artist:someone"], "rating": "safe", "source": " https://x ", "description": ""}"#;
        assert_eq!(
            parse_json(plain).unwrap(),
            Sidecar {
                tags: vec!["cat".into(), "artist:someone".into()],
                rating: Some(Rating::General),
                source: Some("https://x".into()),
                description: None,
            }
        );
        let danbooru = r#"{"tag_string": "a b c", "tag_string_general": "a", "tag_string_artist": "b",
                            "tag_string_character": "c", "rating": "q"}"#;
        let sidecar = parse_json(danbooru).unwrap();
        assert_eq!(sidecar.tags, ["a", "artist:b", "character:c"]);
        assert_eq!(sidecar.rating, Some(Rating::Questionable));
        let e621 = r#"{"tags": {"general": ["x"], "artist": ["y"], "species": ["z"]}}"#;
        assert_eq!(parse_json(e621).unwrap().tags, ["x", "artist:y", "z"]);
        assert_eq!(parse_json(r#"{"tags": "p q"}"#).unwrap().tags, ["p", "q"]);
        assert!(parse_json("[1]").is_err());
        assert!(parse_json("nope").is_err());
    }

    #[test]
    fn merges_sidecars() {
        let mut json = parse_json(r#"{"tags": ["a"], "source": "s"}"#).unwrap();
        json.merge(parse_txt("b\nrating:g"));
        assert_eq!(json.tags, ["a", "b"]);
        assert_eq!(json.rating, Some(Rating::General));
        assert_eq!(json.source.as_deref(), Some("s"));
    }
}
