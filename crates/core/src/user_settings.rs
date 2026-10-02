//! A user's own preferences, stored as JSON in `users.settings`.
//!
//! Reading is lenient: a missing, unknown or malformed field falls back to
//! its default, so a bad stored value can never lock anyone out.

use serde_json::{Map, Value};

/// Posts-per-page choices offered on the settings page.
pub const PER_PAGE_CHOICES: [u32; 5] = [20, 40, 60, 100, 200];

/// Light or dark colours. Every theme has both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Follow the browser's light or dark preference.
    #[default]
    System,
    Light,
    Dark,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::System, Mode::Light, Mode::Dark];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::System => "system",
            Mode::Light => "light",
            Mode::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }
}

/// The theme built into the stylesheet, used when no other is chosen.
pub const DEFAULT_THEME: &str = "default";

/// Whether `name` can name a theme: 1 to 32 lowercase letters, digits and
/// dashes, as in its file name (`themes/<name>.css`), and not a mode's name.
pub fn is_theme_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && Mode::parse(name).is_none()
}

/// Longest custom stylesheet a user may save, in bytes.
pub const MAX_CUSTOM_CSS: usize = 64 * 1024;

/// Whether `name` can name a time zone: an IANA name such as
/// `Europe/Berlin` or `UTC`. Whether the zone exists is checked where the
/// time zone database is.
pub fn is_time_zone_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
        && !name.split('/').any(|part| part.is_empty() || part == "..")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSettings {
    /// `None` uses the site's default.
    pub per_page: Option<u32>,
    pub mode: Mode,
    /// The colour theme's name; `None` uses the site's default. It may name
    /// a theme the site no longer has, which also falls back to the default.
    pub theme: Option<String>,
    /// `None` until the user saves one: the site's default applies.
    pub blacklist: Option<String>,
    /// Only general-rated posts, everywhere.
    pub safe_mode: bool,
    /// Post pages show the original image rather than the resized sample.
    pub original_images: bool,
    /// Searches include deleted posts, for those who may see them.
    pub show_deleted: bool,
    /// Grids use the larger thumbnails.
    pub large_thumbnails: bool,
    /// Blacklisted posts stay in grids, blurred, rather than being left out.
    pub blur_blacklisted: bool,
    /// For displayed dates; `None` for UTC.
    pub time_zone: Option<String>,
    /// Post pages leave out comments.
    pub hide_comments: bool,
    /// Notifications are emailed too (to a confirmed address).
    pub email_notifications: bool,
    /// Tag autocomplete in search and tag fields.
    pub autocomplete: bool,
    /// Keyboard shortcuts.
    pub shortcuts: bool,
    /// A stylesheet applied after the site's; empty for none.
    pub custom_css: String,
    /// The language pages are shown in (a tag like `en-US`); `None`
    /// follows the browser. It may name one the site no longer has.
    pub language: Option<String>,
}

impl Default for UserSettings {
    fn default() -> Self {
        Self {
            per_page: None,
            mode: Mode::default(),
            theme: None,
            blacklist: None,
            safe_mode: false,
            original_images: false,
            show_deleted: false,
            large_thumbnails: false,
            blur_blacklisted: false,
            time_zone: None,
            hide_comments: false,
            email_notifications: false,
            autocomplete: true,
            shortcuts: true,
            custom_css: String::new(),
            language: None,
        }
    }
}

/// Whether `tag` could be a language tag: letters, digits and dashes.
pub fn is_language_tag(tag: &str) -> bool {
    (2..=35).contains(&tag.len()) && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Boolean settings that are on unless turned off. The rest are off
/// unless turned on.
const ON_BY_DEFAULT: [&str; 2] = ["autocomplete", "shortcuts"];

impl UserSettings {
    pub fn from_json(value: &Value) -> Self {
        let per_page = value
            .get("per_page")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| PER_PAGE_CHOICES.contains(n));
        let theme = value.get("theme").and_then(Value::as_str);
        // Before themes had colours, `theme` held the mode.
        let mode = value
            .get("mode")
            .and_then(Value::as_str)
            .or(theme)
            .and_then(Mode::parse)
            .unwrap_or_default();
        let theme = theme.filter(|name| is_theme_name(name)).map(str::to_owned);
        let blacklist = value
            .get("blacklist")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let flag = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_bool)
                .unwrap_or(ON_BY_DEFAULT.contains(&key))
        };
        let time_zone = value
            .get("time_zone")
            .and_then(Value::as_str)
            .filter(|name| is_time_zone_name(name))
            .map(str::to_owned);
        let custom_css = value
            .get("custom_css")
            .and_then(Value::as_str)
            .filter(|css| css.len() <= MAX_CUSTOM_CSS)
            .unwrap_or_default()
            .to_owned();
        let language = value
            .get("language")
            .and_then(Value::as_str)
            .filter(|tag| is_language_tag(tag))
            .map(str::to_owned);
        Self {
            per_page,
            mode,
            theme,
            blacklist,
            safe_mode: flag("safe_mode"),
            original_images: flag("original_images"),
            show_deleted: flag("show_deleted"),
            large_thumbnails: flag("large_thumbnails"),
            blur_blacklisted: flag("blur_blacklisted"),
            time_zone,
            hide_comments: flag("hide_comments"),
            email_notifications: flag("email_notifications"),
            autocomplete: flag("autocomplete"),
            shortcuts: flag("shortcuts"),
            custom_css,
            language,
        }
    }

    /// `previous` with these settings written over it, keeping fields this
    /// version doesn't know about.
    pub fn to_json(&self, previous: &Value) -> Value {
        let mut map = previous.as_object().cloned().unwrap_or_else(Map::new);
        match self.per_page {
            Some(n) => map.insert("per_page".into(), n.into()),
            None => map.remove("per_page"),
        };
        map.insert("mode".into(), self.mode.as_str().into());
        match &self.theme {
            Some(name) => map.insert("theme".into(), name.as_str().into()),
            None => map.remove("theme"),
        };
        match &self.blacklist {
            Some(text) => map.insert("blacklist".into(), text.as_str().into()),
            None => map.remove("blacklist"),
        };
        for (key, on) in [
            ("safe_mode", self.safe_mode),
            ("original_images", self.original_images),
            ("show_deleted", self.show_deleted),
            ("large_thumbnails", self.large_thumbnails),
            ("blur_blacklisted", self.blur_blacklisted),
            ("hide_comments", self.hide_comments),
            ("email_notifications", self.email_notifications),
            ("autocomplete", self.autocomplete),
            ("shortcuts", self.shortcuts),
        ] {
            // Defaults aren't stored, so they can change.
            if on == ON_BY_DEFAULT.contains(&key) {
                map.remove(key);
            } else {
                map.insert(key.into(), on.into());
            }
        }
        match &self.time_zone {
            Some(name) => map.insert("time_zone".into(), name.as_str().into()),
            None => map.remove("time_zone"),
        };
        if self.custom_css.is_empty() {
            map.remove("custom_css");
        } else {
            map.insert("custom_css".into(), self.custom_css.as_str().into());
        }
        match &self.language {
            Some(tag) => map.insert("language".into(), tag.as_str().into()),
            None => map.remove("language"),
        };
        Value::Object(map)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_leniently() {
        assert_eq!(UserSettings::from_json(&json!({})), UserSettings::default());
        assert_eq!(
            UserSettings::from_json(&json!({ "per_page": "lots", "theme": 3, "mode": "dim" })),
            UserSettings::default()
        );
        assert_eq!(
            UserSettings::from_json(&json!({ "per_page": 7 })).per_page,
            None,
            "not a choice"
        );
        assert_eq!(
            UserSettings::from_json(&json!({ "theme": "../main" })).theme,
            None,
            "not a theme name"
        );
        let settings =
            UserSettings::from_json(&json!({ "per_page": 100, "mode": "dark", "theme": "sakura" }));
        assert_eq!(
            settings,
            UserSettings {
                per_page: Some(100),
                mode: Mode::Dark,
                theme: Some("sakura".into()),
                ..UserSettings::default()
            }
        );
    }

    #[test]
    fn reads_the_mode_from_the_old_theme_field() {
        let settings = UserSettings::from_json(&json!({ "theme": "dark" }));
        assert_eq!((settings.mode, settings.theme), (Mode::Dark, None));
    }

    #[test]
    fn writes_over_the_previous_value() {
        let previous = json!({ "per_page": 60, "theme": "dark", "future": true });
        let settings = UserSettings {
            mode: Mode::Light,
            ..UserSettings::default()
        };
        assert_eq!(
            settings.to_json(&previous),
            json!({ "mode": "light", "future": true })
        );
        assert_eq!(
            UserSettings::from_json(&settings.to_json(&previous)),
            settings
        );
        let themed = UserSettings {
            theme: Some("ocean".into()),
            ..settings
        };
        assert_eq!(UserSettings::from_json(&themed.to_json(&previous)), themed);
    }

    #[test]
    fn stores_only_changed_flags() {
        let settings = UserSettings {
            safe_mode: true,
            shortcuts: false,
            time_zone: Some("Europe/Berlin".into()),
            custom_css: "body { color: red }".into(),
            ..UserSettings::default()
        };
        let stored = settings.to_json(&json!({ "autocomplete": false }));
        assert_eq!(
            stored,
            json!({
                "mode": "system",
                "safe_mode": true,
                "shortcuts": false,
                "time_zone": "Europe/Berlin",
                "custom_css": "body { color: red }",
            })
        );
        assert_eq!(UserSettings::from_json(&stored), settings);
        assert!(UserSettings::from_json(&json!({})).autocomplete);
        assert_eq!(
            UserSettings::from_json(&json!({ "time_zone": "../etc/passwd" })).time_zone,
            None
        );
    }

    #[test]
    fn checks_time_zone_names() {
        for good in [
            "UTC",
            "Europe/Berlin",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+9",
        ] {
            assert!(is_time_zone_name(good), "{good}");
        }
        for bad in ["", "a b", "Europe//Berlin", "../x", "/UTC", &"a".repeat(65)] {
            assert!(!is_time_zone_name(bad), "{bad}");
        }
    }

    #[test]
    fn checks_theme_names() {
        assert!(is_theme_name("sakura"));
        assert!(is_theme_name("high-contrast-2"));
        for bad in ["", "Sakura", "a/b", "a.css", "dark", &"a".repeat(33)] {
            assert!(!is_theme_name(bad), "{bad}");
        }
    }
}
