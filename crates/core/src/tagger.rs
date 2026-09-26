//! The tagger's models and what their output means. Running them is the
//! `moekura-tagger` crate's job.
//!
//! The models are SmilingWolf's WD taggers: ONNX image classifiers trained
//! on Danbooru, published with a `selected_tags.csv` naming each output.
//! Their tag categories are Danbooru's, which Moekura's default ones
//! share, plus category 9 for the four ratings.

use crate::posts::Rating;

/// A model's files, pinned to one revision so their checksums hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub model_url: &'static str,
    pub model_sha256: &'static str,
    pub tags_url: &'static str,
    pub tags_sha256: &'static str,
}

/// The model used unless the configuration names another: the fastest of
/// the v3 taggers, and the one small machines can run.
pub const DEFAULT_MODEL: &str = "wd-vit-tagger-v3";

/// Every v3 tagger shares one tag list.
const V3_TAGS_SHA256: &str = "298633d94d0031d2081c0893f29c82eab7f0df00b08483ba8f29d1e979441217";

macro_rules! wd_v3 {
    ($name:literal, $revision:literal, $model_sha256:literal) => {
        Preset {
            name: $name,
            model_url: concat!(
                "https://huggingface.co/SmilingWolf/",
                $name,
                "/resolve/",
                $revision,
                "/model.onnx"
            ),
            model_sha256: $model_sha256,
            tags_url: concat!(
                "https://huggingface.co/SmilingWolf/",
                $name,
                "/resolve/",
                $revision,
                "/selected_tags.csv"
            ),
            tags_sha256: V3_TAGS_SHA256,
        }
    };
}

/// Models known by name, smallest first.
pub const PRESETS: &[Preset] = &[
    wd_v3!(
        "wd-vit-tagger-v3",
        "7f6b584d0bd3f55c4531f14ba3d4761b2bccdc0f",
        "35f23693620b668f4d53fd3c62bf65e40af739bc52c7eb0fbc49258b58d065b6"
    ),
    wd_v3!(
        "wd-convnext-tagger-v3",
        "d39e46de298d27340111b64965e20b8185c407e6",
        "1b8a7abf13d9b8368267df47501d523789c4aeae66b2296ad98483239dfa32eb"
    ),
    wd_v3!(
        "wd-swinv2-tagger-v3",
        "627aef95638667ddcaa3ac8ae625e88ea5b02f51",
        "e6774bff34d43bd49f75a47db4ef217dce701c9847b546523eb85ff6dbba1db1"
    ),
    wd_v3!(
        "wd-eva02-large-tagger-v3",
        "b25b82a03f7282e41aa2f257a52c7583b710bd1c",
        "9e768793060c7939b277ccb382783e8670e8a042d29d77aa736be0c8cc898bfc"
    ),
];

pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

/// The tag list's category for ratings.
const RATING_CATEGORY: i16 = 9;

/// What one of the model's outputs stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    Rating(Rating),
    Tag {
        /// As the model spells it (Danbooru's names).
        name: String,
        /// Danbooru's category id, which is Moekura's too.
        category_id: i16,
    },
}

/// Reads a `selected_tags.csv`: a header, then `tag_id,name,category,count`
/// per output, in output order.
pub fn parse_labels(csv: &str) -> Result<Vec<Label>, String> {
    let mut lines = csv.lines();
    let header = lines.next().ok_or("the tag list is empty")?;
    let columns = split_csv(header)?;
    let column = |name: &str| {
        columns
            .iter()
            .position(|c| c == name)
            .ok_or_else(|| format!("the tag list has no `{name}` column"))
    };
    let (name_at, category_at) = (column("name")?, column("category")?);
    let mut labels = Vec::new();
    for (n, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        let at = |message: &str| format!("line {}: {message}", n + 2);
        let fields = split_csv(line).map_err(|e| at(&e))?;
        let (Some(name), Some(category)) = (fields.get(name_at), fields.get(category_at)) else {
            return Err(at("missing columns"));
        };
        let category: i16 = category.parse().map_err(|_| at("bad category"))?;
        labels.push(if category == RATING_CATEGORY {
            let rating = match name.as_str() {
                "general" => Rating::General,
                "sensitive" => Rating::Sensitive,
                "questionable" => Rating::Questionable,
                "explicit" => Rating::Explicit,
                other => return Err(at(&format!("unknown rating `{other}`"))),
            };
            Label::Rating(rating)
        } else {
            Label::Tag {
                name: name.clone(),
                category_id: category,
            }
        });
    }
    if labels.is_empty() {
        return Err("the tag list names no tags".into());
    }
    Ok(labels)
}

/// Splits one CSV line, with `"quoted, ""escaped"""` fields.
fn split_csv(line: &str) -> Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (false, ',') => fields.push(std::mem::take(&mut field)),
            (false, '"') if field.is_empty() => quoted = true,
            (true, '"') if chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            (true, '"') => quoted = false,
            (_, c) => field.push(c),
        }
    }
    if quoted {
        return Err("unterminated quote".into());
    }
    fields.push(field);
    Ok(fields)
}

/// What the model made of an image.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    /// The likeliest rating, with its score.
    pub rating: Option<(Rating, f32)>,
    /// Tags scoring at least the floor asked for, best first, with the
    /// model's category.
    pub tags: Vec<(String, i16, f32)>,
}

/// Pairs `scores` (one per label, 0 to 1) with `labels`, keeping tags
/// that score at least `floor`.
pub fn interpret(labels: &[Label], scores: &[f32], floor: f32) -> Prediction {
    let mut rating: Option<(Rating, f32)> = None;
    let mut tags = Vec::new();
    for (label, &score) in labels.iter().zip(scores) {
        match label {
            Label::Rating(r) => {
                if rating.is_none_or(|(_, best)| score > best) {
                    rating = Some((*r, score));
                }
            }
            Label::Tag { name, category_id } if score >= floor => {
                tags.push((name.clone(), *category_id, score));
            }
            Label::Tag { .. } => {}
        }
    }
    tags.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    Prediction { rating, tags }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CSV: &str = "tag_id,name,category,count
9999999,general,9,1589178
9999998,sensitive,9,3994361
9999997,questionable,9,892496
9999996,explicit,9,706509
470575,1girl,0,5113288
612924,\"don't_say_\"\"lazy\"\"\",0,1062
1234,hatsune_miku,4,100
";

    #[test]
    fn reads_tag_lists() {
        let labels = parse_labels(CSV).unwrap();
        assert_eq!(labels.len(), 7);
        assert_eq!(labels[0], Label::Rating(Rating::General));
        assert_eq!(labels[3], Label::Rating(Rating::Explicit));
        assert_eq!(
            labels[5],
            Label::Tag {
                name: "don't_say_\"lazy\"".into(),
                category_id: 0
            }
        );
        assert_eq!(
            labels[6],
            Label::Tag {
                name: "hatsune_miku".into(),
                category_id: 4
            }
        );
    }

    #[test]
    fn refuses_broken_tag_lists() {
        assert!(parse_labels("").is_err());
        assert!(parse_labels("tag_id,name,category,count\n").is_err());
        assert!(parse_labels("id,label\n1,a\n").is_err());
        let err = parse_labels("tag_id,name,category,count\n1,a,x,1\n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        let err = parse_labels("tag_id,name,category,count\n1,cute,9,1\n").unwrap_err();
        assert!(err.contains("unknown rating"), "{err}");
        assert!(parse_labels("tag_id,name,category,count\n1,\"open,0,1\n").is_err());
    }

    #[test]
    fn interprets_scores() {
        let labels = parse_labels(CSV).unwrap();
        let scores = [0.1, 0.7, 0.15, 0.05, 0.99, 0.2, 0.6];
        let prediction = interpret(&labels, &scores, 0.3);
        assert_eq!(prediction.rating, Some((Rating::Sensitive, 0.7)));
        assert_eq!(
            prediction.tags,
            [
                ("1girl".to_owned(), 0, 0.99),
                ("hatsune_miku".to_owned(), 4, 0.6)
            ]
        );
    }

    #[test]
    fn presets_are_pinned() {
        assert!(preset(DEFAULT_MODEL).is_some());
        for p in PRESETS {
            for url in [p.model_url, p.tags_url] {
                assert!(
                    url.starts_with("https://huggingface.co/SmilingWolf/"),
                    "{url}"
                );
                assert!(url.contains(p.name), "{url}");
            }
            for sum in [p.model_sha256, p.tags_sha256] {
                assert!(
                    sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()),
                    "{sum}"
                );
            }
        }
    }
}
