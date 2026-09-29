//! Defences against spam accounts: which email domains may sign up, and
//! where a captcha is asked for.

use serde::{Deserialize, Serialize};

/// The longest list of email domains.
pub const MAX_DOMAINS: usize = 10_000;

/// Which email domains may be used for accounts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmailDomains {
    pub mode: DomainMode,
    /// Lowercase domains; each covers its subdomains too.
    pub domains: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainMode {
    /// Any domain but those listed.
    #[default]
    Block,
    /// Only the domains listed (an empty list allows any).
    Allow,
}

impl EmailDomains {
    /// Domains from text, one per line or separated by spaces or commas,
    /// lowercased, with any `@` in front dropped, sorted and deduplicated.
    pub fn parse_list(text: &str) -> Vec<String> {
        let mut domains: Vec<String> = text
            .split(|c: char| c.is_whitespace() || c == ',')
            .map(|d| {
                d.trim_start_matches('@')
                    .trim_end_matches('.')
                    .to_lowercase()
            })
            .filter(|d| !d.is_empty())
            .collect();
        domains.sort();
        domains.dedup();
        domains
    }

    /// Whether an account may use `email`.
    pub fn allows(&self, email: &str) -> bool {
        if self.domains.is_empty() {
            return true;
        }
        let domain = email
            .rsplit_once('@')
            .map_or("", |(_, d)| d)
            .trim_end_matches('.')
            .to_lowercase();
        let listed = self.domains.iter().any(|d| {
            domain == *d
                || domain
                    .strip_suffix(d.as_str())
                    .is_some_and(|rest| rest.ends_with('.'))
        });
        match self.mode {
            DomainMode::Block => !listed,
            DomainMode::Allow => listed,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.domains.len() > MAX_DOMAINS {
            return Err(format!("list at most {MAX_DOMAINS} domains"));
        }
        for domain in &self.domains {
            let valid = domain.len() <= 253
                && domain.contains('.')
                && domain.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || !c.is_ascii())
                });
            if !valid || *domain != domain.to_lowercase() {
                return Err(format!("`{domain}` isn't a domain like example.com"));
            }
        }
        Ok(())
    }
}

/// Where the captcha (`auth.captcha` in the configuration) is asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptchaSettings {
    /// On the sign-up form.
    pub sign_up: bool,
    /// On comments by accounts younger than this many days; 0 for none.
    pub comment_account_days: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_allows_domains_and_their_subdomains() {
        let blocked = EmailDomains {
            mode: DomainMode::Block,
            domains: EmailDomains::parse_list("Spam.example\n@junk.test, spam.example"),
        };
        assert_eq!(blocked.domains, ["junk.test", "spam.example"]);
        assert!(blocked.validate().is_ok());
        assert!(!blocked.allows("a@spam.example"));
        assert!(!blocked.allows("a@mail.SPAM.example."));
        assert!(blocked.allows("a@notspam.example"));
        assert!(blocked.allows("a@example.com"));

        let allowed = EmailDomains {
            mode: DomainMode::Allow,
            domains: vec!["uni.example".into()],
        };
        assert!(allowed.allows("a@cs.uni.example"));
        assert!(!allowed.allows("a@example.com"));
        assert!(EmailDomains::default().allows("a@anything.example"));
    }

    #[test]
    fn refuses_what_isnt_a_domain() {
        for bad in ["localhost", "a..b", "exa mple.com", "a/b.com"] {
            let rules = EmailDomains {
                mode: DomainMode::Block,
                domains: vec![bad.into()],
            };
            assert!(rules.validate().is_err(), "{bad}");
        }
    }
}
