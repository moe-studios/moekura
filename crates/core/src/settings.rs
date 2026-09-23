//! Site settings: runtime options admins change without a restart, stored
//! in the database. Infrastructure options live in [`crate::config`].

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SITE_NAME_MAX_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteSettings {
    pub site_name: String,
    pub registration_mode: RegistrationMode,
    /// New uploads wait in the approval queue unless the uploader's role
    /// has `UploadWithoutApproval`.
    pub upload_approval: bool,
    /// Blacklist for visitors and for users who never saved their own
    /// (see [`crate::blacklist`]); e.g. `rating:e` to hide explicit posts.
    pub default_blacklist: String,
}

impl Default for SiteSettings {
    fn default() -> Self {
        Self {
            site_name: "uwubooru".to_owned(),
            registration_mode: RegistrationMode::Open,
            upload_approval: false,
            default_blacklist: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
    /// Anyone can register and use their account immediately.
    Open,
    /// Registration requires an invite code.
    Invite,
    /// Anyone can register, but accounts wait for staff approval.
    Approval,
    /// No self-registration; admins create accounts.
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingError {
    #[error("unknown setting `{0}`")]
    UnknownKey(String),
    #[error("invalid value for `{key}`: {message}")]
    InvalidValue { key: String, message: String },
}

impl SiteSettings {
    /// Names of every setting, alphabetically.
    pub fn keys() -> Vec<String> {
        match serde_json::to_value(Self::default()) {
            Ok(Value::Object(map)) => map.keys().cloned().collect(),
            _ => unreachable!("SiteSettings serializes to an object"),
        }
    }

    /// A copy with `key` set to `value`, if the key exists and the result
    /// is valid.
    pub fn with_value(&self, key: &str, value: Value) -> Result<Self, SettingError> {
        let mut map = self.to_map();
        if !map.contains_key(key) {
            return Err(SettingError::UnknownKey(key.to_owned()));
        }
        map.insert(key.to_owned(), value);
        let invalid = |message: String| SettingError::InvalidValue {
            key: key.to_owned(),
            message,
        };
        let updated: Self =
            serde_json::from_value(Value::Object(map)).map_err(|e| invalid(e.to_string()))?;
        updated.validate().map_err(invalid)?;
        Ok(updated)
    }

    /// Builds settings from stored key/value rows. Unknown keys (from removed
    /// settings) and invalid values are skipped so a bad row can't take the
    /// site down; skipped keys are returned for logging.
    pub fn from_rows(rows: impl IntoIterator<Item = (String, Value)>) -> (Self, Vec<String>) {
        let mut settings = Self::default();
        let mut skipped = Vec::new();
        for (key, value) in rows {
            match settings.with_value(&key, value) {
                Ok(updated) => settings = updated,
                Err(_) => skipped.push(key),
            }
        }
        (settings, skipped)
    }

    pub fn to_map(&self) -> Map<String, Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(map)) => map,
            _ => unreachable!("SiteSettings serializes to an object"),
        }
    }

    fn validate(&self) -> Result<(), String> {
        let name = self.site_name.trim();
        if name.is_empty() {
            return Err("must not be empty".into());
        }
        if name.chars().count() > SITE_NAME_MAX_LEN {
            return Err(format!("must be at most {SITE_NAME_MAX_LEN} characters"));
        }
        crate::blacklist::Blacklist::parse(&self.default_blacklist).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn lists_keys() {
        assert_eq!(
            SiteSettings::keys(),
            [
                "default_blacklist",
                "registration_mode",
                "site_name",
                "upload_approval"
            ]
        );
    }

    #[test]
    fn sets_known_keys() {
        let settings = SiteSettings::default()
            .with_value("registration_mode", json!("closed"))
            .unwrap();
        assert_eq!(settings.registration_mode, RegistrationMode::Closed);
    }

    #[test]
    fn rejects_unknown_keys_and_bad_values() {
        let defaults = SiteSettings::default();
        assert_eq!(
            defaults.with_value("nope", json!(1)),
            Err(SettingError::UnknownKey("nope".into()))
        );
        assert!(matches!(
            defaults.with_value("registration_mode", json!("sometimes")),
            Err(SettingError::InvalidValue { .. })
        ));
        assert!(matches!(
            defaults.with_value("site_name", json!("   ")),
            Err(SettingError::InvalidValue { .. })
        ));
    }

    #[test]
    fn from_rows_skips_bad_rows() {
        let rows = [
            ("site_name".to_owned(), json!("my booru")),
            ("retired_setting".to_owned(), json!(true)),
            ("registration_mode".to_owned(), json!(42)),
        ];
        let (settings, skipped) = SiteSettings::from_rows(rows);
        assert_eq!(settings.site_name, "my booru");
        assert_eq!(settings.registration_mode, RegistrationMode::Open);
        assert_eq!(skipped, ["retired_setting", "registration_mode"]);
    }
}
