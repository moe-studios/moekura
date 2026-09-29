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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserSettings {
    /// `None` uses the site's default.
    pub per_page: Option<u32>,
    pub mode: Mode,
    /// The colour theme's name; `None` uses the site's default. It may name
    /// a theme the site no longer has, which also falls back to the default.
    pub theme: Option<String>,
    /// `None` until the user saves one: the site's default applies.
    pub blacklist: Option<String>,
}

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
        Self {
            per_page,
            mode,
            theme,
            blacklist,
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
                blacklist: None,
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
            per_page: None,
            mode: Mode::Light,
            theme: None,
            blacklist: None,
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
    fn checks_theme_names() {
        assert!(is_theme_name("sakura"));
        assert!(is_theme_name("high-contrast-2"));
        for bad in ["", "Sakura", "a/b", "a.css", "dark", &"a".repeat(33)] {
            assert!(!is_theme_name(bad), "{bad}");
        }
    }
}
