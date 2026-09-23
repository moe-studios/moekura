//! A user's own preferences, stored as JSON in `users.settings`.
//!
//! Reading is lenient: a missing, unknown or malformed field falls back to
//! its default, so a bad stored value can never lock anyone out.

use serde_json::{Map, Value};

/// Posts-per-page choices offered on the settings page.
pub const PER_PAGE_CHOICES: [u32; 5] = [20, 40, 60, 100, 200];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the browser's light or dark preference.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::System, Theme::Light, Theme::Dark];

    pub fn as_str(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserSettings {
    /// `None` uses the site's default.
    pub per_page: Option<u32>,
    pub theme: Theme,
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
        let theme = value
            .get("theme")
            .and_then(Value::as_str)
            .and_then(Theme::parse)
            .unwrap_or_default();
        let blacklist = value
            .get("blacklist")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Self {
            per_page,
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
        map.insert("theme".into(), self.theme.as_str().into());
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
            UserSettings::from_json(&json!({ "per_page": "lots", "theme": 3 })),
            UserSettings::default()
        );
        assert_eq!(
            UserSettings::from_json(&json!({ "per_page": 7 })).per_page,
            None,
            "not a choice"
        );
        let settings = UserSettings::from_json(&json!({ "per_page": 100, "theme": "dark" }));
        assert_eq!(
            settings,
            UserSettings {
                per_page: Some(100),
                theme: Theme::Dark,
                blacklist: None,
            }
        );
    }

    #[test]
    fn writes_over_the_previous_value() {
        let previous = json!({ "per_page": 60, "future": true });
        let settings = UserSettings {
            per_page: None,
            theme: Theme::Light,
            blacklist: None,
        };
        assert_eq!(
            settings.to_json(&previous),
            json!({ "theme": "light", "future": true })
        );
        assert_eq!(
            UserSettings::from_json(&settings.to_json(&previous)),
            settings
        );
    }
}
