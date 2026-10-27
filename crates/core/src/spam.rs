//! Defences against spam accounts: which email domains may sign up,
//! where a captcha is asked for, and what writing is held for review.

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

/// The longest list of spam words.
pub const MAX_WORDS: usize = 1_000;

/// Which comments, forum posts and messages are held for the staff to
/// review instead of appearing (see [`SpamFilter::check`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpamFilter {
    pub mode: FilterMode,
    /// Links from accounts younger than this many days are held; 0 for
    /// never.
    pub link_account_days: u32,
    /// Links from accounts that have fewer than this many comments, forum
    /// posts and uploads out are held too, however old the account: a
    /// sleeper account's first link waits like a new one's. 0 for never.
    pub link_min_posts: u32,
    /// Text its writer already posted this many times in the past day is
    /// held; 0 for never.
    pub max_repeats: u32,
    /// Lowercase words and phrases that hold any text containing them.
    pub words: Vec<String>,
}

impl Default for SpamFilter {
    fn default() -> Self {
        Self {
            mode: FilterMode::Auto,
            link_account_days: 3,
            link_min_posts: 1,
            max_repeats: 3,
            words: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    /// On, unless the site is private.
    #[default]
    Auto,
    On,
    Off,
}

/// Why writing was held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    /// Links, from a new account.
    Links,
    /// The same text, again and again.
    Repeated(u32),
    /// One of the spam words.
    Word(String),
}

impl std::fmt::Display for Hold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hold::Links => f.write_str("links from a new account, or one that hasn't posted yet"),
            Hold::Repeated(times) => write!(f, "posted {times} times already today"),
            Hold::Word(word) => write!(f, "contains “{word}”"),
        }
    }
}

impl SpamFilter {
    /// Whether the filter runs, on a private site or a public one.
    pub fn is_on(&self, private: bool) -> bool {
        match self.mode {
            FilterMode::Auto => !private,
            FilterMode::On => true,
            FilterMode::Off => false,
        }
    }

    /// Words from text, one per line, folded as text is when checked
    /// (see [`fold`]), sorted and deduplicated.
    pub fn parse_words(text: &str) -> Vec<String> {
        let mut words: Vec<String> = text
            .lines()
            .map(|w| fold(w).trim().to_owned())
            .filter(|w| !w.is_empty())
            .collect();
        words.sort();
        words.dedup();
        words
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.words.len() > MAX_WORDS {
            return Err(format!("list at most {MAX_WORDS} spam words"));
        }
        if self.words.iter().any(|w| w.chars().count() > 100) {
            return Err("spam words must be at most 100 characters".into());
        }
        Ok(())
    }

    /// Whether to hold `text`, by an account `account_days` old with
    /// `posts` comments, forum posts and uploads out, that already posted
    /// the same text (as [`fold`]ed, letters and digits only) `repeats`
    /// times in the past day.
    pub fn check(&self, text: &str, account_days: i64, posts: i64, repeats: i64) -> Option<Hold> {
        let folded = fold(text);
        let tokens = tokens(&folded);
        if let Some(word) = self
            .words
            .iter()
            .find(|w| contains_word(&folded, &tokens, w))
        {
            return Some(Hold::Word(word.clone()));
        }
        if self.max_repeats > 0 && repeats >= i64::from(self.max_repeats) {
            return Some(Hold::Repeated(u32::try_from(repeats).unwrap_or(u32::MAX)));
        }
        let has_link = ["http://", "https://", "www."]
            .iter()
            .any(|start| folded.contains(start));
        let untried = account_days < i64::from(self.link_account_days)
            || posts < i64::from(self.link_min_posts);
        if has_link && untried {
            return Some(Hold::Links);
        }
        None
    }
}

/// Text as the spam filter compares it: look-alike letters folded
/// together (compatibility forms such as full-width or styled letters,
/// accents dropped), invisible characters removed, lowercased.
pub fn fold(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    use unicode_normalization::char::is_combining_mark;
    let invisible = |c: char| {
        matches!(c,
            '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}'
            | '\u{17B5}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
            | '\u{FFA0}')
    };
    text.nfkd()
        .filter(|c| !is_combining_mark(*c) && !invisible(*c))
        .collect::<String>()
        .nfkc()
        .collect::<String>()
        .to_lowercase()
}

/// The words of folded text, with runs of single letters spaced or
/// dotted apart (`c a s i n o`, `c.a.s.i.n.o`) joined back into one.
fn tokens(folded: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut letters = String::new();
    for word in folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if word.chars().count() == 1 {
            letters.push_str(word);
            continue;
        }
        if !letters.is_empty() {
            words.push(std::mem::take(&mut letters));
        }
        words.push(word.to_owned());
    }
    if !letters.is_empty() {
        words.push(letters);
    }
    words
}

/// Whether a script writes words without spaces between them, so that
/// spam words in it can't be told apart by word boundaries.
fn unspaced(c: char) -> bool {
    matches!(c,
        '\u{0E00}'..='\u{0EFF}'   // Thai, Lao
        | '\u{1000}'..='\u{109F}' // Myanmar
        | '\u{1780}'..='\u{17FF}' // Khmer
        | '\u{3040}'..='\u{30FF}' // Hiragana, Katakana
        | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' // Han
    )
}

/// Whether folded text (with its [`tokens`]) has spam word `word` in it:
/// as whole words, so `ass` doesn't hold "class", or anywhere for words in
/// scripts without spaces.
fn contains_word(folded: &str, tokens: &[String], word: &str) -> bool {
    if word.chars().any(unspaced) {
        return folded.contains(word);
    }
    let wanted: Vec<&str> = word
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if wanted.is_empty() {
        return false;
    }
    // Spelled out letter by letter, a word is one token.
    let squashed: String = wanted.concat();
    tokens
        .windows(wanted.len())
        .any(|w| w.iter().map(String::as_str).eq(wanted.iter().copied()))
        || tokens.contains(&squashed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_links_repeats_and_words() {
        let filter = SpamFilter {
            words: SpamFilter::parse_words("Cheap Pills\n\n casino \ncasino"),
            ..SpamFilter::default()
        };
        assert_eq!(filter.words, ["casino", "cheap pills"]);
        assert!(filter.validate().is_ok());
        assert_eq!(filter.check("hello", 0, 0, 0), None);
        assert_eq!(
            filter.check("see https://spam.example", 1, 5, 0),
            Some(Hold::Links)
        );
        assert_eq!(filter.check("see https://art.example", 30, 5, 0), None);
        // An old account that never posted is held like a new one.
        assert_eq!(
            filter.check("see https://art.example", 300, 0, 0),
            Some(Hold::Links)
        );
        assert_eq!(filter.check("hello", 30, 5, 3), Some(Hold::Repeated(3)));
        assert_eq!(filter.check("hello", 30, 5, 2), None);
        assert_eq!(
            filter.check("Buy CHEAP PILLS now", 900, 5, 0),
            Some(Hold::Word("cheap pills".into()))
        );
        assert_eq!(Hold::Word("casino".into()).to_string(), "contains “casino”");

        assert!(filter.is_on(false));
        assert!(!filter.is_on(true));
        let on = SpamFilter {
            mode: FilterMode::On,
            ..SpamFilter::default()
        };
        assert!(on.is_on(true));
        let off = SpamFilter {
            mode: FilterMode::Off,
            link_account_days: 0,
            link_min_posts: 0,
            max_repeats: 0,
            words: Vec::new(),
        };
        assert!(!off.is_on(false));
        assert_eq!(off.check("https://x.example", 0, 0, 99), None);
    }

    #[test]
    fn spam_words_see_through_disguises_but_not_into_other_words() {
        let filter = SpamFilter {
            words: SpamFilter::parse_words("casino\ncheap pills\nass\n賭博"),
            ..SpamFilter::default()
        };
        let held = |text: &str| filter.check(text, 900, 5, 0).is_some();
        // Look-alike letters, accents, hidden characters, spacing.
        for text in [
            "ＣＡＳＩＮＯ night",
            "𝐜𝐚𝐬𝐢𝐧𝐨",
            "cásínò",
            "cas\u{200B}ino",
            "c a s i n o",
            "c.a.s.i.n.o!",
            "cheap-pills",
            "CHEAP\npills",
            "best 賭博サイト",
        ] {
            assert!(held(text), "{text}");
        }
        // Whole words only.
        for text in [
            "a class act",
            "assess it",
            "casinos? no",
            "cheap, but no pills",
        ] {
            assert!(!held(text), "{text}");
        }
        assert_eq!(fold("Ｃáfé\u{FEFF}"), "cafe");
    }

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
