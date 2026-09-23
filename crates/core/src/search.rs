//! Search query syntax.
//!
//! A query is whitespace-separated terms, as on Danbooru:
//!
//! - `tag` must be present, `-tag` must be absent, and `~a ~b` means at
//!   least one of `a` and `b`
//! - `*` in a tag is a wildcard (`long_*`, `*_hair`); an included wildcard
//!   matches posts with any of the tags it expands to
//! - `name:value` terms are filters on post properties (metatags), e.g.
//!   `rating:e,q`, `score:>=10`, `width:1920..`, `date:2026-01`; most can
//!   be negated with `-`
//!
//! [`Query::parse`] only checks syntax; resolving tags and users is the
//! planner's job.

use std::fmt;

use time::{Date, Duration, Month};

use crate::posts::Rating;
use crate::tags::{RESERVED_PREFIXES, TagName, TagNameError, normalize};

/// File types as stored in `media_assets.media_type`.
pub const FILETYPES: &[&str] = &["jpeg", "png", "gif", "webp", "avif", "jxl", "mp4", "webm"];

/// Metatags this version understands. Other reserved prefixes
/// ([`RESERVED_PREFIXES`]) are refused as not supported yet.
pub const METATAGS: &[&str] = &[
    "id", "rating", "status", "user", "score", "favcount", "width", "height", "mpixels", "ratio",
    "filesize", "duration", "date", "filetype", "md5", "parent", "tagcount", "order", "limit",
    "fav", "ordfav", "similar",
];

/// Category names accepted, and ignored, in front of a search tag
/// (`artist:name` searches for `name`).
const CATEGORY_PREFIXES: &[&str] = &["general", "artist", "copyright", "character", "meta"];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TagTerm {
    Name(TagName),
    /// A normalised pattern containing at least one `*`.
    Wildcard(String),
}

impl fmt::Display for TagTerm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TagTerm::Name(name) => f.write_str(name.as_str()),
            TagTerm::Wildcard(pattern) => f.write_str(pattern),
        }
    }
}

/// A comparison against a number (or date, or size).
#[derive(Debug, Clone, PartialEq)]
pub enum Bound<T> {
    Eq(T),
    Lt(T),
    Le(T),
    Gt(T),
    Ge(T),
    /// Inclusive at both ends.
    Between(T, T),
    In(Vec<T>),
}

impl<T: fmt::Display> fmt::Display for Bound<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Bound::Eq(v) => write!(f, "{v}"),
            Bound::Lt(v) => write!(f, "<{v}"),
            Bound::Le(v) => write!(f, "<={v}"),
            Bound::Gt(v) => write!(f, ">{v}"),
            Bound::Ge(v) => write!(f, ">={v}"),
            Bound::Between(a, b) => write!(f, "{a}..{b}"),
            Bound::In(values) => {
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{v}")?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusFilter {
    Pending,
    Active,
    Flagged,
    Deleted,
    /// Everything the viewer may see, deleted posts included.
    Any,
}

impl StatusFilter {
    pub fn as_str(self) -> &'static str {
        match self {
            StatusFilter::Pending => "pending",
            StatusFilter::Active => "active",
            StatusFilter::Flagged => "flagged",
            StatusFilter::Deleted => "deleted",
            StatusFilter::Any => "any",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentFilter {
    /// Posts without a parent.
    None,
    /// Posts with a parent.
    Any,
    /// A post and its children.
    Of(i64),
}

/// A condition on a post property.
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Id(Bound<i64>),
    Rating(Vec<Rating>),
    Status(StatusFilter),
    /// Uploaded by this user (name as typed).
    User(String),
    Score(Bound<i64>),
    FavCount(Bound<i64>),
    Width(Bound<i64>),
    Height(Bound<i64>),
    /// Width × height in millions of pixels.
    Mpixels(Bound<f64>),
    /// Width ÷ height; equality allows ±0.01.
    Ratio(Bound<f64>),
    /// Bytes.
    FileSize(Bound<i64>),
    /// Seconds.
    Duration(Bound<f64>),
    /// Uploaded on or after `from` and before `until` (UTC days).
    Date {
        from: Option<Date>,
        until: Option<Date>,
    },
    FileType(Vec<String>),
    Md5(Vec<[u8; 16]>),
    Parent(ParentFilter),
    TagCount(Bound<i64>),
    /// Favorited by this user (name as typed).
    Fav(String),
    /// Looks like this post (perceptual hash), the post included.
    Similar(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub negated: bool,
    pub filter: Filter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    IdDesc,
    IdAsc,
    ScoreDesc,
    ScoreAsc,
    FavCountDesc,
    FavCountAsc,
    MpixelsDesc,
    MpixelsAsc,
    FileSizeDesc,
    FileSizeAsc,
    /// Widest first.
    Landscape,
    /// Tallest first.
    Portrait,
    DurationDesc,
    DurationAsc,
    TagCountDesc,
    TagCountAsc,
    Random,
    /// Newest favorites of [`Query::ordfav`] first (`ordfav:name`).
    Favorited,
}

impl Order {
    /// Names accepted by `order:`, canonical name first for each order.
    pub const NAMES: &[(&'static str, Order)] = &[
        ("id", Order::IdDesc),
        ("id_desc", Order::IdDesc),
        ("id_asc", Order::IdAsc),
        ("score", Order::ScoreDesc),
        ("score_desc", Order::ScoreDesc),
        ("score_asc", Order::ScoreAsc),
        ("favcount", Order::FavCountDesc),
        ("favcount_desc", Order::FavCountDesc),
        ("favcount_asc", Order::FavCountAsc),
        ("mpixels", Order::MpixelsDesc),
        ("mpixels_desc", Order::MpixelsDesc),
        ("mpixels_asc", Order::MpixelsAsc),
        ("filesize", Order::FileSizeDesc),
        ("filesize_desc", Order::FileSizeDesc),
        ("filesize_asc", Order::FileSizeAsc),
        ("landscape", Order::Landscape),
        ("portrait", Order::Portrait),
        ("duration", Order::DurationDesc),
        ("duration_desc", Order::DurationDesc),
        ("duration_asc", Order::DurationAsc),
        ("tagcount", Order::TagCountDesc),
        ("tagcount_desc", Order::TagCountDesc),
        ("tagcount_asc", Order::TagCountAsc),
        ("random", Order::Random),
    ];

    pub fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(_, order)| *order == self)
            .map_or("id", |(name, _)| name)
    }
}

/// A parsed search.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Query {
    /// Each must match.
    pub all: Vec<TagTerm>,
    /// At least one must match (the `~` terms).
    pub any: Vec<TagTerm>,
    /// None may match.
    pub none: Vec<TagTerm>,
    pub conditions: Vec<Condition>,
    pub order: Option<Order>,
    pub limit: Option<u32>,
    /// With `ordfav:name`: the user whose favorites these are.
    pub ordfav: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    #[error("`{term}`: {message}")]
    InvalidValue { term: String, message: String },
    #[error("`{term}`: the tag {error}")]
    InvalidTag { term: String, error: TagNameError },
    #[error("`{0}:` searches aren't supported yet")]
    Unsupported(String),
    #[error("`{0}` can't be negated")]
    CantNegate(String),
    #[error("`{0}`: metatags can't be combined with `~`")]
    OrMetatag(String),
    #[error("`{0}`: use either `-` or `~`, not both")]
    NegatedOr(String),
    #[error("`{0}` is missing a tag")]
    Empty(String),
    #[error("`{0}` needs something besides `*`")]
    BareWildcard(String),
}

impl Query {
    pub fn parse(input: &str) -> Result<Self, SearchError> {
        let mut query = Query::default();
        for word in input.split_whitespace() {
            query.add(word)?;
        }
        Ok(query)
    }

    /// Tag terms and filters, the measure for query complexity limits.
    pub fn term_count(&self) -> usize {
        self.all.len() + self.any.len() + self.none.len() + self.conditions.len()
    }

    pub fn is_empty(&self) -> bool {
        *self == Query::default()
    }

    /// The status asked for with a (not negated) `status:` filter.
    pub fn status(&self) -> Option<StatusFilter> {
        self.conditions.iter().rev().find_map(|c| match c.filter {
            Filter::Status(status) if !c.negated => Some(status),
            _ => None,
        })
    }

    /// Every tag term, in or out.
    pub fn tag_terms(&self) -> impl Iterator<Item = &TagTerm> {
        self.all.iter().chain(&self.any).chain(&self.none)
    }

    fn add(&mut self, word: &str) -> Result<(), SearchError> {
        let (negated, rest) = match word.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, word),
        };
        let (or, rest) = match rest.strip_prefix('~') {
            Some(rest) => (true, rest),
            None => (false, rest),
        };
        if negated && or {
            return Err(SearchError::NegatedOr(word.into()));
        }
        if rest.is_empty() {
            return Err(SearchError::Empty(word.into()));
        }

        let mut tag = rest;
        if let Some((prefix, value)) = rest.split_once(':') {
            let prefix = prefix.to_lowercase();
            if METATAGS.contains(&prefix.as_str()) {
                if or {
                    return Err(SearchError::OrMetatag(word.into()));
                }
                return self.add_metatag(word, negated, &prefix, &value.to_lowercase());
            }
            if CATEGORY_PREFIXES.contains(&prefix.as_str()) && !value.is_empty() {
                tag = value;
            } else if RESERVED_PREFIXES.contains(&prefix.as_str()) {
                return Err(SearchError::Unsupported(prefix));
            }
        }

        let term = tag_term(word, tag)?;
        let list = if negated {
            &mut self.none
        } else if or {
            &mut self.any
        } else {
            &mut self.all
        };
        if !list.contains(&term) {
            list.push(term);
        }
        Ok(())
    }

    fn add_metatag(
        &mut self,
        word: &str,
        negated: bool,
        name: &str,
        value: &str,
    ) -> Result<(), SearchError> {
        let invalid = |message: &str| SearchError::InvalidValue {
            term: word.into(),
            message: message.into(),
        };
        const NUMBER: &str = "expected a number like 5, >=5, <5, 5..10 or 1,2,3";
        let filter = match name {
            "order" | "limit" | "ordfav" if negated => {
                return Err(SearchError::CantNegate(word.into()));
            }
            "ordfav" => {
                if value.is_empty() {
                    return Err(invalid("expected a user name"));
                }
                self.ordfav = Some(value.into());
                self.order = Some(Order::Favorited);
                return Ok(());
            }
            "similar" => Filter::Similar(value.parse().map_err(|_| invalid("expected a post id"))?),
            "fav" => {
                if value.is_empty() {
                    return Err(invalid("expected a user name"));
                }
                Filter::Fav(value.into())
            }
            "order" => {
                let order = Order::NAMES
                    .iter()
                    .find(|(n, _)| *n == value)
                    .map(|(_, order)| *order)
                    .ok_or_else(|| invalid("unknown order; try id, score, favcount or random"))?;
                self.order = Some(order);
                return Ok(());
            }
            "limit" => {
                let limit = value
                    .parse::<u32>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| invalid("expected a positive number"))?;
                self.limit = Some(limit);
                return Ok(());
            }
            "id" => Filter::Id(bound(value, int).ok_or_else(|| invalid(NUMBER))?),
            "score" => Filter::Score(bound(value, int).ok_or_else(|| invalid(NUMBER))?),
            "favcount" => Filter::FavCount(bound(value, int).ok_or_else(|| invalid(NUMBER))?),
            "width" => Filter::Width(bound(value, int).ok_or_else(|| invalid(NUMBER))?),
            "height" => Filter::Height(bound(value, int).ok_or_else(|| invalid(NUMBER))?),
            "tagcount" => Filter::TagCount(bound(value, int).ok_or_else(|| invalid(NUMBER))?),
            "mpixels" => Filter::Mpixels(bound(value, float).ok_or_else(|| invalid(NUMBER))?),
            "duration" => Filter::Duration(
                bound(value, float).ok_or_else(|| invalid("expected seconds, like >30"))?,
            ),
            "ratio" => Filter::Ratio(
                bound(value, ratio).ok_or_else(|| invalid("expected a ratio like 16:9 or 1.5"))?,
            ),
            "filesize" => Filter::FileSize(
                file_size(value).ok_or_else(|| invalid("expected a size like 500kb or >2mb"))?,
            ),
            "date" => {
                let (from, until) = date_range(value)
                    .ok_or_else(|| invalid("expected a date like 2026-01-31, 2026-01 or 2026"))?;
                Filter::Date { from, until }
            }
            "rating" => Filter::Rating(
                list(value, |v| v.parse().ok())
                    .ok_or_else(|| invalid("expected ratings like g, s, q or e"))?,
            ),
            "status" => Filter::Status(match value {
                "pending" => StatusFilter::Pending,
                "active" => StatusFilter::Active,
                "flagged" => StatusFilter::Flagged,
                "deleted" => StatusFilter::Deleted,
                "any" | "all" => StatusFilter::Any,
                _ => return Err(invalid("expected pending, active, flagged, deleted or any")),
            }),
            "user" => {
                if value.is_empty() {
                    return Err(invalid("expected a user name"));
                }
                Filter::User(value.into())
            }
            "filetype" => Filter::FileType(
                list(value, |v| {
                    let v = if v == "jpg" { "jpeg" } else { v };
                    FILETYPES.contains(&v).then(|| v.to_owned())
                })
                .ok_or_else(|| invalid("expected file types like png, gif or webm"))?,
            ),
            "md5" => Filter::Md5(
                list(value, md5).ok_or_else(|| invalid("expected 32 hexadecimal digits"))?,
            ),
            "parent" => Filter::Parent(match value {
                "none" => ParentFilter::None,
                "any" => ParentFilter::Any,
                id => ParentFilter::Of(
                    id.parse()
                        .map_err(|_| invalid("expected a post id, none or any"))?,
                ),
            }),
            _ => unreachable!("every name in METATAGS is handled"),
        };
        let condition = Condition { negated, filter };
        if !self.conditions.contains(&condition) {
            self.conditions.push(condition);
        }
        Ok(())
    }
}

fn tag_term(word: &str, raw: &str) -> Result<TagTerm, SearchError> {
    if raw.contains('*') {
        let pattern = normalize(raw);
        if pattern.chars().all(|c| c == '*') {
            return Err(SearchError::BareWildcard(word.into()));
        }
        // Check what's left as if it were a tag.
        return match TagName::parse(&pattern.replace('*', "x")) {
            Ok(_) => Ok(TagTerm::Wildcard(pattern)),
            Err(error) => Err(SearchError::InvalidTag {
                term: word.into(),
                error,
            }),
        };
    }
    TagName::parse(raw)
        .map(TagTerm::Name)
        .map_err(|error| SearchError::InvalidTag {
            term: word.into(),
            error,
        })
}

fn int(s: &str) -> Option<i64> {
    s.parse().ok()
}

fn float(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn ratio(s: &str) -> Option<f64> {
    match s.split_once(':') {
        Some((w, h)) => {
            let (w, h) = (float(w)?, float(h)?);
            (h > 0.0).then(|| w / h)
        }
        None => float(s),
    }
}

fn md5(s: &str) -> Option<[u8; 16]> {
    let bytes = hex::decode(s).ok()?;
    bytes.try_into().ok()
}

fn list<T>(value: &str, one: impl Fn(&str) -> Option<T>) -> Option<Vec<T>> {
    let items: Option<Vec<T>> = value.split(',').map(|v| one(v.trim())).collect();
    items.filter(|items| !items.is_empty())
}

/// `5`, `<5`, `<=5`, `>5`, `>=5`, `5..10`, `..10`, `5..`, `1,2,3`.
fn bound<T>(value: &str, one: impl Fn(&str) -> Option<T>) -> Option<Bound<T>> {
    if let Some(v) = value.strip_prefix(">=") {
        return one(v).map(Bound::Ge);
    }
    if let Some(v) = value.strip_prefix("<=") {
        return one(v).map(Bound::Le);
    }
    if let Some(v) = value.strip_prefix('>') {
        return one(v).map(Bound::Gt);
    }
    if let Some(v) = value.strip_prefix('<') {
        return one(v).map(Bound::Lt);
    }
    if let Some((a, b)) = value.split_once("..") {
        return match (a.is_empty(), b.is_empty()) {
            (true, true) => None,
            (true, false) => one(b).map(Bound::Le),
            (false, true) => one(a).map(Bound::Ge),
            (false, false) => Some(Bound::Between(one(a)?, one(b)?)),
        };
    }
    if value.contains(',') {
        return list(value, one).map(Bound::In);
    }
    one(value).map(Bound::Eq)
}

/// A size in bytes: `1024`, `500kb`, `1.5mb`, `2gb`. An exact size with a
/// unit matches ±5%, since `1mb` is rarely meant to the byte.
fn file_size(value: &str) -> Option<Bound<i64>> {
    fn bytes(s: &str) -> Option<(f64, bool)> {
        let s = s.trim_end_matches('b');
        let (number, unit) = match s.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
            Some((i, _)) => s.split_at(i),
            None => (s, ""),
        };
        let multiplier = match unit {
            "" => 1.0,
            "k" => 1024.0,
            "m" => 1024.0 * 1024.0,
            "g" => 1024.0 * 1024.0 * 1024.0,
            _ => return None,
        };
        Some((float(number)? * multiplier, !unit.is_empty()))
    }
    let parsed = bound(value, bytes)?;
    let round = |v: f64| v.round() as i64;
    Some(match parsed {
        Bound::Eq((v, true)) => Bound::Between(round(v * 0.95), round(v * 1.05)),
        Bound::Eq((v, false)) => Bound::Eq(round(v)),
        Bound::Lt((v, _)) => Bound::Lt(round(v)),
        Bound::Le((v, _)) => Bound::Le(round(v)),
        Bound::Gt((v, _)) => Bound::Gt(round(v)),
        Bound::Ge((v, _)) => Bound::Ge(round(v)),
        Bound::Between((a, _), (b, _)) => Bound::Between(round(a), round(b)),
        Bound::In(values) => Bound::In(values.into_iter().map(|(v, _)| round(v)).collect()),
    })
}

/// The days covered by `2026-01-31`, `2026-01` or `2026`: first day and
/// the day after the last.
fn period(s: &str) -> Option<(Date, Date)> {
    let parts: Vec<&str> = s.split('-').collect();
    let number = |s: &str| s.parse::<i32>().ok();
    let (year, month, day) = match parts[..] {
        [y] => (number(y)?, None, None),
        [y, m] => (number(y)?, Some(number(m)?), None),
        [y, m, d] => (number(y)?, Some(number(m)?), Some(number(d)?)),
        _ => return None,
    };
    if !(1..=9999).contains(&year) {
        return None;
    }
    let month_of = |m: i32| Month::try_from(u8::try_from(m).ok()?).ok();
    match (month, day) {
        (None, _) => Some((
            Date::from_calendar_date(year, Month::January, 1).ok()?,
            Date::from_calendar_date(year + 1, Month::January, 1).ok()?,
        )),
        (Some(m), None) => {
            let month = month_of(m)?;
            let start = Date::from_calendar_date(year, month, 1).ok()?;
            let next = if month == Month::December {
                Date::from_calendar_date(year + 1, Month::January, 1).ok()?
            } else {
                Date::from_calendar_date(year, month.next(), 1).ok()?
            };
            Some((start, next))
        }
        (Some(m), Some(d)) => {
            let start = Date::from_calendar_date(year, month_of(m)?, u8::try_from(d).ok()?).ok()?;
            Some((start, start + Duration::days(1)))
        }
    }
}

fn date_range(value: &str) -> Option<(Option<Date>, Option<Date>)> {
    Some(match bound(value, period)? {
        Bound::Eq((start, end)) => (Some(start), Some(end)),
        Bound::Gt((_, end)) => (Some(end), None),
        Bound::Ge((start, _)) => (Some(start), None),
        Bound::Lt((start, _)) => (None, Some(start)),
        Bound::Le((_, end)) => (None, Some(end)),
        Bound::Between((start, _), (_, end)) => (Some(start), Some(end)),
        Bound::In(_) => return None,
    })
}

impl fmt::Display for Condition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negated {
            f.write_str("-")?;
        }
        let join = |items: Vec<String>| items.join(",");
        match &self.filter {
            Filter::Id(b) => write!(f, "id:{b}"),
            Filter::Rating(ratings) => write!(
                f,
                "rating:{}",
                join(ratings.iter().map(|r| r.code().to_owned()).collect())
            ),
            Filter::Status(status) => write!(f, "status:{}", status.as_str()),
            Filter::User(name) => write!(f, "user:{name}"),
            Filter::Score(b) => write!(f, "score:{b}"),
            Filter::FavCount(b) => write!(f, "favcount:{b}"),
            Filter::Width(b) => write!(f, "width:{b}"),
            Filter::Height(b) => write!(f, "height:{b}"),
            Filter::Mpixels(b) => write!(f, "mpixels:{b}"),
            Filter::Ratio(b) => write!(f, "ratio:{b}"),
            Filter::FileSize(b) => write!(f, "filesize:{b}"),
            Filter::Duration(b) => write!(f, "duration:{b}"),
            Filter::Date { from, until } => {
                let last = |d: &Date| *d - Duration::days(1);
                match (from, until) {
                    (Some(a), Some(b)) if *a == last(b) => write!(f, "date:{a}"),
                    (Some(a), Some(b)) => write!(f, "date:{a}..{}", last(b)),
                    (Some(a), None) => write!(f, "date:>={a}"),
                    (None, Some(b)) => write!(f, "date:<{b}"),
                    (None, None) => f.write_str("date:>=0001-01-01"),
                }
            }
            Filter::FileType(types) => write!(f, "filetype:{}", types.join(",")),
            Filter::Md5(hashes) => {
                write!(f, "md5:{}", join(hashes.iter().map(hex::encode).collect()))
            }
            Filter::Parent(ParentFilter::None) => f.write_str("parent:none"),
            Filter::Parent(ParentFilter::Any) => f.write_str("parent:any"),
            Filter::Parent(ParentFilter::Of(id)) => write!(f, "parent:{id}"),
            Filter::TagCount(b) => write!(f, "tagcount:{b}"),
            Filter::Fav(name) => write!(f, "fav:{name}"),
            Filter::Similar(id) => write!(f, "similar:{id}"),
        }
    }
}

/// The normalised query: included tags, `~` tags, excluded tags, filters,
/// then order and limit.
impl fmt::Display for Query {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut terms: Vec<String> = Vec::with_capacity(self.term_count() + 2);
        terms.extend(self.all.iter().map(ToString::to_string));
        terms.extend(self.any.iter().map(|t| format!("~{t}")));
        terms.extend(self.none.iter().map(|t| format!("-{t}")));
        terms.extend(self.conditions.iter().map(ToString::to_string));
        match (self.order, &self.ordfav) {
            (Some(Order::Favorited), Some(user)) => terms.push(format!("ordfav:{user}")),
            (Some(order), _) => terms.push(format!("order:{}", order.name())),
            (None, _) => {}
        }
        if let Some(limit) = self.limit {
            terms.push(format!("limit:{limit}"));
        }
        f.write_str(&terms.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use time::macros::date;

    use super::*;

    fn parse(input: &str) -> Query {
        Query::parse(input).unwrap_or_else(|e| panic!("{input}: {e}"))
    }

    fn filter(input: &str) -> Filter {
        let query = parse(input);
        assert_eq!(query.conditions.len(), 1, "{input}");
        query.conditions[0].filter.clone()
    }

    fn error(input: &str) -> String {
        match Query::parse(input) {
            Ok(query) => panic!("{input} parsed as {query:?}"),
            Err(e) => e.to_string(),
        }
    }

    fn name(s: &str) -> TagTerm {
        TagTerm::Name(TagName::parse(s).unwrap())
    }

    #[test]
    fn tags_negation_and_or() {
        let query = parse("Long_Hair -solo ~cat ~dog  long_hair artist:someone re:zero");
        assert_eq!(
            query.all,
            [name("long_hair"), name("someone"), name("re:zero")]
        );
        assert_eq!(query.any, [name("cat"), name("dog")]);
        assert_eq!(query.none, [name("solo")]);
        assert!(query.conditions.is_empty());
        assert_eq!(query.term_count(), 6);
        assert!(parse("   ").is_empty());
    }

    #[test]
    fn wildcards() {
        let query = parse("long_* -*_HAIR ~a*b");
        assert_eq!(query.all, [TagTerm::Wildcard("long_*".into())]);
        assert_eq!(query.none, [TagTerm::Wildcard("*_hair".into())]);
        assert_eq!(query.any, [TagTerm::Wildcard("a*b".into())]);
        assert_eq!(error("*"), "`*` needs something besides `*`");
        assert_eq!(error("-**"), "`-**` needs something besides `*`");
    }

    #[test]
    fn numeric_bounds() {
        assert_eq!(filter("score:5"), Filter::Score(Bound::Eq(5)));
        assert_eq!(filter("score:-5"), Filter::Score(Bound::Eq(-5)));
        assert_eq!(filter("score:>=10"), Filter::Score(Bound::Ge(10)));
        assert_eq!(filter("width:<1920"), Filter::Width(Bound::Lt(1920)));
        assert_eq!(filter("height:>1080"), Filter::Height(Bound::Gt(1080)));
        assert_eq!(filter("id:<=100"), Filter::Id(Bound::Le(100)));
        assert_eq!(filter("id:5..10"), Filter::Id(Bound::Between(5, 10)));
        assert_eq!(filter("id:..10"), Filter::Id(Bound::Le(10)));
        assert_eq!(filter("id:5.."), Filter::Id(Bound::Ge(5)));
        assert_eq!(filter("id:1,2,3"), Filter::Id(Bound::In(vec![1, 2, 3])));
        assert_eq!(filter("favcount:>0"), Filter::FavCount(Bound::Gt(0)));
        assert_eq!(filter("tagcount:<5"), Filter::TagCount(Bound::Lt(5)));
        assert_eq!(filter("mpixels:>2.5"), Filter::Mpixels(Bound::Gt(2.5)));
        assert_eq!(
            filter("duration:10..30"),
            Filter::Duration(Bound::Between(10.0, 30.0))
        );
        for bad in ["score:abc", "id:..", "width:>", "id:1,x", "mpixels:nan"] {
            assert!(
                error(bad).contains("expected a number"),
                "{bad}: {}",
                error(bad)
            );
        }
    }

    #[test]
    fn ratios_and_sizes() {
        let Filter::Ratio(Bound::Eq(r)) = filter("ratio:16:9") else {
            panic!()
        };
        assert!((r - 16.0 / 9.0).abs() < 1e-9);
        assert_eq!(filter("ratio:>1"), Filter::Ratio(Bound::Gt(1.0)));
        assert!(error("ratio:1:0").contains("expected a ratio"));

        assert_eq!(filter("filesize:1024"), Filter::FileSize(Bound::Eq(1024)));
        assert_eq!(
            filter("filesize:>2mb"),
            Filter::FileSize(Bound::Gt(2 * 1024 * 1024))
        );
        assert_eq!(
            filter("filesize:1.5k"),
            Filter::FileSize(Bound::Between(1459, 1613))
        );
        assert_eq!(
            filter("filesize:1kb..1MB"),
            Filter::FileSize(Bound::Between(1024, 1024 * 1024))
        );
        assert!(error("filesize:5tb").contains("expected a size"));
    }

    #[test]
    fn dates() {
        let range = |input: &str| match filter(input) {
            Filter::Date { from, until } => (from, until),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            range("date:2026-01-31"),
            (Some(date!(2026 - 01 - 31)), Some(date!(2026 - 02 - 01)))
        );
        assert_eq!(
            range("date:2026-12"),
            (Some(date!(2026 - 12 - 01)), Some(date!(2027 - 01 - 01)))
        );
        assert_eq!(range("date:>2026"), (Some(date!(2027 - 01 - 01)), None));
        assert_eq!(range("date:>=2026-02"), (Some(date!(2026 - 02 - 01)), None));
        assert_eq!(range("date:<2026-02"), (None, Some(date!(2026 - 02 - 01))));
        assert_eq!(range("date:<=2026-02"), (None, Some(date!(2026 - 03 - 01))));
        assert_eq!(
            range("date:2026-01..2026-03"),
            (Some(date!(2026 - 01 - 01)), Some(date!(2026 - 04 - 01)))
        );
        for bad in [
            "date:2026-13",
            "date:2026-02-30",
            "date:soon",
            "date:2026,2027",
        ] {
            assert!(error(bad).contains("expected a date"), "{bad}");
        }
    }

    #[test]
    fn enumerated_filters() {
        assert_eq!(
            filter("rating:e,Q"),
            Filter::Rating(vec![Rating::Explicit, Rating::Questionable])
        );
        assert_eq!(
            filter("rating:general"),
            Filter::Rating(vec![Rating::General])
        );
        assert!(error("rating:x").contains("expected ratings"));
        assert_eq!(filter("status:all"), Filter::Status(StatusFilter::Any));
        assert!(error("status:gone").contains("expected pending"));
        assert_eq!(
            filter("filetype:jpg,WEBM"),
            Filter::FileType(vec!["jpeg".into(), "webm".into()])
        );
        assert!(error("filetype:bmp").contains("expected file types"));
        assert_eq!(filter("user:Alice"), Filter::User("alice".into()));
        assert_eq!(filter("parent:none"), Filter::Parent(ParentFilter::None));
        assert_eq!(filter("parent:12"), Filter::Parent(ParentFilter::Of(12)));
        assert!(error("parent:x").contains("expected a post id"));
        let hash = "d41d8cd98f00b204e9800998ecf8427e";
        assert_eq!(
            filter(&format!("md5:{}", hash.to_uppercase())),
            Filter::Md5(vec![md5(hash).unwrap()])
        );
        assert!(error("md5:abc").contains("32 hexadecimal"));
    }

    #[test]
    fn favorites() {
        let query = parse("cat -fav:Bob ordfav:Alice");
        assert_eq!(
            query.conditions,
            [Condition {
                negated: true,
                filter: Filter::Fav("bob".into())
            }]
        );
        assert_eq!(
            (query.order, query.ordfav.as_deref()),
            (Some(Order::Favorited), Some("alice"))
        );
        assert_eq!(query.to_string(), "cat -fav:bob ordfav:alice");
        assert_eq!(parse(&query.to_string()), query);
        // A later order: replaces ordfav's order.
        assert_eq!(parse("ordfav:a order:score").order, Some(Order::ScoreDesc));
        assert!(error("-ordfav:a").contains("can't be negated"));
        assert!(error("fav:").contains("expected a user name"));
    }

    #[test]
    fn negation_order_and_limit() {
        let query = parse("-rating:e -status:deleted order:score_asc limit:20 order:favcount");
        assert_eq!(
            query.conditions,
            [
                Condition {
                    negated: true,
                    filter: Filter::Rating(vec![Rating::Explicit])
                },
                Condition {
                    negated: true,
                    filter: Filter::Status(StatusFilter::Deleted)
                },
            ]
        );
        // The last order wins.
        assert_eq!(query.order, Some(Order::FavCountDesc));
        assert_eq!(query.limit, Some(20));
        assert_eq!(query.status(), None);
        assert_eq!(
            parse("status:pending").status(),
            Some(StatusFilter::Pending)
        );

        assert_eq!(error("-order:id"), "`-order:id` can't be negated");
        assert!(error("order:best").contains("unknown order"));
        assert!(error("limit:0").contains("positive number"));
        assert_eq!(
            error("~rating:e"),
            "`~rating:e`: metatags can't be combined with `~`"
        );
    }

    #[test]
    fn malformed_terms() {
        assert_eq!(error("-~a"), "`-~a`: use either `-` or `~`, not both");
        assert_eq!(error("-"), "`-` is missing a tag");
        assert_eq!(error("pool:12"), "`pool:` searches aren't supported yet");
        assert_eq!(filter("similar:12"), Filter::Similar(12));
        assert!(error("similar:x").contains("expected a post id"));
        assert_eq!(
            error("a\u{7}b"),
            "`a\u{7}b`: the tag may not contain control characters"
        );
        // A metatag name with nothing after the colon is still a metatag.
        assert!(error("rating:").contains("expected ratings"));
    }

    #[test]
    fn prints_the_normalised_query() {
        let input = "-solo Long_Hair ~b ~a rating:e,s order:score_desc -user:X date:2026-01 id:5.. \
                     filesize:<1mb limit:10 date:2026-01..2026-02 date:>=2026";
        let query = parse(input);
        let printed = query.to_string();
        assert_eq!(
            printed,
            "long_hair ~b ~a -solo rating:e,s -user:x date:2026-01-01..2026-01-31 id:>=5 \
             filesize:<1048576 date:2026-01-01..2026-02-28 date:>=2026-01-01 order:score limit:10"
        );
        // Printing is stable: the printed query parses to the same query.
        assert_eq!(parse(&printed), query);
        assert_eq!(
            parse(&parse("date:2026-01-05").to_string()),
            parse("date:2026-01-05")
        );
    }

    #[test]
    fn order_names_are_unique_and_canonical_first() {
        for (name, order) in Order::NAMES {
            assert_eq!(parse(&format!("order:{name}")).order, Some(*order));
            assert!(
                Order::NAMES
                    .iter()
                    .any(|(n, o)| o == order && *n == order.name())
            );
        }
    }
}
