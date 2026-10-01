//! Site settings: runtime options admins change without a restart, stored
//! in the database. Infrastructure options live in [`crate::config`].

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const SITE_NAME_MAX_LEN: usize = 64;
/// The longest [`SiteSettings::site_description`], in characters.
pub const DESCRIPTION_MAX_LEN: usize = 300;
/// The longest [`SiteSettings::rules`], in characters.
pub const RULES_MAX_LEN: usize = 20_000;
/// The most [`SiteSettings::footer_links`].
pub const MAX_FOOTER_LINKS: usize = 12;
/// The longest footer link text, in characters.
pub const FOOTER_LABEL_MAX_LEN: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SiteSettings {
    pub site_name: String,
    /// A sentence or two about the site, for the footer, search engines
    /// and link previews. Empty: none.
    pub site_description: String,
    /// The storage key of the logo shown beside the site's name (uploaded
    /// from the settings page). Empty: none.
    pub logo: String,
    /// The site's content rules, in markup, shown at `/rules` and linked
    /// from the footer, sign-up and uploads. Empty: no rules page.
    pub rules: String,
    /// Extra links in the footer (a Discord server, a donation page, …).
    pub footer_links: Vec<FooterLink>,
    pub registration_mode: RegistrationMode,
    /// New accounts must follow a link sent to their email address before
    /// they can log in. Only applies when mail is configured.
    pub email_verification: bool,
    /// New uploads wait in the approval queue unless the uploader's role
    /// has `UploadWithoutApproval`.
    pub upload_approval: bool,
    /// Pending upload limits grow with a user's approved uploads and
    /// shrink with their deleted ones (see [`crate::uploads`]).
    pub upload_limit_scaling: bool,
    /// Members whose record meets `promotion_rules` become contributors,
    /// checked hourly (see [`crate::promotion`]).
    pub auto_promotion: bool,
    pub promotion_rules: crate::promotion::Rules,
    /// Link previews (OpenGraph, oEmbed) show questionable and explicit
    /// posts' images too. Off: only general and sensitive ones.
    pub preview_all_ratings: bool,
    /// Blacklist for visitors and for users who never saved their own
    /// (see [`crate::blacklist`]); e.g. `rating:e` to hide explicit posts.
    pub default_blacklist: String,
    /// The only ratings logged-out visitors see, anywhere: searches, post
    /// pages, feeds and the APIs. Empty: all of them.
    pub visitor_ratings: Vec<crate::posts::Rating>,
    /// Colour theme for visitors and for users who haven't picked one. A
    /// theme the site doesn't have falls back to the built-in default.
    pub default_theme: String,
    /// What the tagger's suggestions are used for (see
    /// [`crate::tagger::TaggerSettings`]).
    pub tagger: crate::tagger::TaggerSettings,
    /// Days an account's addresses are kept after they were last seen
    /// (for staff looking for ban evasion); 0 keeps none.
    pub ip_history_days: u32,
    /// Email domains accounts may, or may not, use.
    pub email_domains: crate::spam::EmailDomains,
    /// Where the captcha is asked for, if one is configured.
    pub captcha: crate::spam::CaptchaSettings,
    /// Reasons offered for deleting, rejecting and flagging posts.
    pub post_reasons: crate::moderation::PostReasons,
    /// Uploads and edits get `artist_request` while a post has no artist
    /// tag and `tagme` while it has few general tags, and lose them again
    /// once they don't apply.
    pub request_tags: bool,
    /// What happens to posts by artists staff have banned.
    pub banned_artists: BannedArtists,
}

/// What banning an artist does, for everyone but those who approve posts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BannedArtists {
    /// Their posts are left out of searches and their pages not found.
    pub hide_posts: bool,
    /// Uploads, and edits adding their tag, are refused.
    pub refuse_uploads: bool,
}

impl Default for BannedArtists {
    fn default() -> Self {
        Self {
            hide_posts: true,
            refuse_uploads: true,
        }
    }
}

/// The longest [`SiteSettings::ip_history_days`].
pub const MAX_IP_HISTORY_DAYS: u32 = 3650;

impl Default for SiteSettings {
    fn default() -> Self {
        Self {
            site_name: "Moekura".to_owned(),
            site_description: String::new(),
            logo: String::new(),
            rules: String::new(),
            footer_links: Vec::new(),
            registration_mode: RegistrationMode::Open,
            email_verification: false,
            upload_approval: false,
            upload_limit_scaling: false,
            auto_promotion: false,
            promotion_rules: crate::promotion::Rules {
                uploads: 50,
                edits: 0,
                account_days: 30,
                max_recent_deletions: 0,
            },
            preview_all_ratings: false,
            default_blacklist: String::new(),
            visitor_ratings: Vec::new(),
            default_theme: crate::user_settings::DEFAULT_THEME.to_owned(),
            tagger: crate::tagger::TaggerSettings::default(),
            ip_history_days: 365,
            email_domains: crate::spam::EmailDomains::default(),
            captcha: crate::spam::CaptchaSettings::default(),
            post_reasons: crate::moderation::PostReasons::default(),
            request_tags: false,
            banned_artists: BannedArtists::default(),
        }
    }
}

/// A link in the footer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FooterLink {
    pub label: String,
    /// An `http(s)` address, or a path on this site.
    pub url: String,
}

impl FooterLink {
    /// Links from text, one per line: the link's text, then its address
    /// (`Discord https://discord.gg/abc`). Blank lines are skipped; a line
    /// that is only an address uses it as its text.
    pub fn parse_list(text: &str) -> Vec<Self> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| match line.rsplit_once(char::is_whitespace) {
                Some((label, url)) => Self {
                    label: label.trim().to_owned(),
                    url: url.to_owned(),
                },
                None => Self {
                    label: line.to_owned(),
                    url: line.to_owned(),
                },
            })
            .collect()
    }

    /// The links as [`Self::parse_list`] reads them.
    pub fn to_list(links: &[Self]) -> String {
        links
            .iter()
            .map(|link| format!("{} {}", link.label, link.url))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn validate(&self) -> Result<(), String> {
        if self.label.is_empty() || self.label.chars().count() > FOOTER_LABEL_MAX_LEN {
            return Err(format!(
                "link text must be 1 to {FOOTER_LABEL_MAX_LEN} characters"
            ));
        }
        let local = self.url.starts_with('/') && !self.url.starts_with("//");
        let web = url::Url::parse(&self.url)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.has_host());
        if !(local || web) || self.url.chars().any(char::is_whitespace) {
            return Err(format!(
                "`{}` isn't an http(s) address or a path on this site",
                self.url
            ));
        }
        Ok(())
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
            Ok(Value::Object(map)) => {
                let mut keys: Vec<String> = map.keys().cloned().collect();
                keys.sort();
                keys
            }
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
        if self.site_description.chars().count() > DESCRIPTION_MAX_LEN {
            return Err(format!(
                "the description must be at most {DESCRIPTION_MAX_LEN} characters"
            ));
        }
        if self.rules.chars().count() > RULES_MAX_LEN {
            return Err(format!(
                "the rules must be at most {RULES_MAX_LEN} characters"
            ));
        }
        // A storage key; the web layer checks it names a real file.
        if !self
            .logo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'-'))
            || self.logo.contains("..")
        {
            return Err("not a stored file".into());
        }
        if self.footer_links.len() > MAX_FOOTER_LINKS {
            return Err(format!("at most {MAX_FOOTER_LINKS} footer links"));
        }
        for link in &self.footer_links {
            link.validate()?;
        }
        crate::blacklist::Blacklist::parse(&self.default_blacklist).map_err(|e| e.to_string())?;
        if !crate::user_settings::is_theme_name(&self.default_theme) {
            return Err("must be a theme's name".into());
        }
        self.tagger.validate()?;
        self.email_domains.validate()?;
        self.post_reasons.validate()?;
        if self.ip_history_days > MAX_IP_HISTORY_DAYS {
            return Err(format!(
                "keep addresses for at most {MAX_IP_HISTORY_DAYS} days"
            ));
        }
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
                "auto_promotion",
                "banned_artists",
                "captcha",
                "default_blacklist",
                "default_theme",
                "email_domains",
                "email_verification",
                "footer_links",
                "ip_history_days",
                "logo",
                "post_reasons",
                "preview_all_ratings",
                "promotion_rules",
                "registration_mode",
                "request_tags",
                "rules",
                "site_description",
                "site_name",
                "tagger",
                "upload_approval",
                "upload_limit_scaling",
                "visitor_ratings"
            ]
        );
    }

    #[test]
    fn sets_known_keys() {
        let settings = SiteSettings::default()
            .with_value("registration_mode", json!("closed"))
            .unwrap();
        assert_eq!(settings.registration_mode, RegistrationMode::Closed);
        let settings = settings
            .with_value("visitor_ratings", json!(["g", "s"]))
            .unwrap();
        assert_eq!(
            settings.visitor_ratings,
            [
                crate::posts::Rating::General,
                crate::posts::Rating::Sensitive
            ]
        );
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
        assert!(matches!(
            defaults.with_value("default_theme", json!("../css/main")),
            Err(SettingError::InvalidValue { .. })
        ));
        let thresholds = json!({ "thresholds": { "general": 0 } });
        assert!(matches!(
            defaults.with_value("tagger", thresholds),
            Err(SettingError::InvalidValue { .. })
        ));
    }

    #[test]
    fn footer_links() {
        let links = FooterLink::parse_list("Our Discord  https://discord.gg/abc\n\n/wiki/help\n");
        assert_eq!(
            links,
            [
                FooterLink {
                    label: "Our Discord".into(),
                    url: "https://discord.gg/abc".into()
                },
                FooterLink {
                    label: "/wiki/help".into(),
                    url: "/wiki/help".into()
                },
            ]
        );
        assert_eq!(FooterLink::parse_list(&FooterLink::to_list(&links)), links);
        let defaults = SiteSettings::default();
        let set = |text: &str| {
            defaults.with_value(
                "footer_links",
                serde_json::to_value(FooterLink::parse_list(text)).unwrap(),
            )
        };
        assert!(set("Help /wiki/help\nDonate https://ko-fi.com/x").is_ok());
        for bad in [
            "Evil javascript:alert(1)",
            "Proto //evil.example",
            "Mail mailto:a@b.c",
        ] {
            assert!(set(bad).is_err(), "{bad}");
        }
        assert!(
            defaults
                .with_value("logo", json!("../../etc/passwd"))
                .is_err()
        );
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
