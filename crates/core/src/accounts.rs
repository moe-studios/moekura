//! Rules for account names, passwords and emails, and password hashing.

use std::fmt;
use std::sync::LazyLock;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

pub const NAME_MIN_LEN: usize = 2;
pub const NAME_MAX_LEN: usize = 32;
pub const PASSWORD_MIN_LEN: usize = 8;
/// Caps hashing work per attempt; long passphrases still fit comfortably.
pub const PASSWORD_MAX_LEN: usize = 256;
pub const EMAIL_MAX_LEN: usize = 254;

/// Names that would collide with routes or read as official.
const RESERVED_NAMES: &[&str] = &[
    "admin",
    "administrator",
    "anonymous",
    "api",
    "login",
    "logout",
    "me",
    "mod",
    "moderator",
    "new",
    "register",
    "root",
    "settings",
    "staff",
    "system",
    "uwubooru",
];

/// A validated account name.
///
/// ASCII letters, digits, `_`, `.` and `-`; no spaces or colons, so names
/// work inside search queries like `user:name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserName(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("must be at least {NAME_MIN_LEN} characters")]
    TooShort,
    #[error("must be at most {NAME_MAX_LEN} characters")]
    TooLong,
    #[error("may only contain letters, digits, underscores, dots and hyphens")]
    InvalidCharacters,
    #[error("may not start or end with a dot or hyphen")]
    BadEdge,
    #[error("may not consist only of digits")]
    AllDigits,
    #[error("is reserved")]
    Reserved,
}

impl UserName {
    pub fn parse(raw: &str) -> Result<Self, NameError> {
        // Checked before the length so non-ASCII input can't skew the count.
        if !raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
        {
            return Err(NameError::InvalidCharacters);
        }
        if raw.len() < NAME_MIN_LEN {
            return Err(NameError::TooShort);
        }
        if raw.len() > NAME_MAX_LEN {
            return Err(NameError::TooLong);
        }
        let edge = |b: u8| matches!(b, b'.' | b'-');
        if edge(raw.as_bytes()[0]) || edge(raw.as_bytes()[raw.len() - 1]) {
            return Err(NameError::BadEdge);
        }
        if raw.bytes().all(|b| b.is_ascii_digit()) {
            return Err(NameError::AllDigits);
        }
        if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(raw)) {
            return Err(NameError::Reserved);
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UserName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    #[error("must be at least {PASSWORD_MIN_LEN} characters")]
    TooShort,
    #[error("must be at most {PASSWORD_MAX_LEN} characters")]
    TooLong,
}

/// Length limits only, following NIST SP 800-63B: no composition rules.
pub fn check_password(password: &str) -> Result<(), PasswordError> {
    let len = password.chars().count();
    if len < PASSWORD_MIN_LEN {
        Err(PasswordError::TooShort)
    } else if len > PASSWORD_MAX_LEN {
        Err(PasswordError::TooLong)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("is not a valid email address")]
pub struct EmailError;

/// A deliberately loose check; only a confirmation mail proves an address.
pub fn check_email(email: &str) -> Result<(), EmailError> {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return Err(EmailError);
    };
    let valid = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && email.len() <= EMAIL_MAX_LEN
        && !email.chars().any(|c| c.is_whitespace() || c.is_control());
    if valid { Ok(()) } else { Err(EmailError) }
}

/// Outcome of checking a password against a stored hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    Invalid,
    /// Correct; `needs_rehash` is set when the hash uses outdated parameters.
    Valid {
        needs_rehash: bool,
    },
}

/// Hashes with argon2id at the OWASP-recommended parameters (19 MiB, t=2,
/// p=1). CPU-heavy: call from a blocking thread.
pub fn hash_password(password: &str) -> String {
    Argon2::default()
        .hash_password(password.as_bytes())
        .expect("argon2 with default parameters cannot fail")
        .to_string()
}

/// CPU-heavy: call from a blocking thread. A malformed stored hash counts
/// as a mismatch.
pub fn verify_password(password: &str, stored_hash: &str) -> Verification {
    let Ok(hash) = PasswordHash::new(stored_hash) else {
        return Verification::Invalid;
    };
    if Argon2::default()
        .verify_password(password.as_bytes(), &hash)
        .is_err()
    {
        return Verification::Invalid;
    }
    let current = argon2::Params::default();
    let needs_rehash = hash.algorithm != argon2::ARGON2ID_IDENT
        || argon2::Params::try_from(&hash).is_ok_and(|p| {
            (p.m_cost(), p.t_cost(), p.p_cost())
                != (current.m_cost(), current.t_cost(), current.p_cost())
        });
    Verification::Valid { needs_rehash }
}

/// Does the same work as a real verification so a login attempt for an
/// unknown account takes as long as one for a known account.
pub fn verify_dummy_password(password: &str) {
    static DUMMY_HASH: LazyLock<String> =
        LazyLock::new(|| hash_password("uwubooru dummy password"));
    let _ = verify_password(password, &DUMMY_HASH);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_reasonable_names() {
        for name in [
            "catherine",
            "Ira_B",
            "a1",
            "user.name",
            "x-y-z",
            "_under",
            "123abc",
        ] {
            assert_eq!(UserName::parse(name).unwrap().as_str(), name);
        }
    }

    #[test]
    fn rejects_bad_names() {
        let cases = [
            ("a", NameError::TooShort),
            ("", NameError::TooShort),
            (&"a".repeat(33), NameError::TooLong),
            ("has space", NameError::InvalidCharacters),
            ("user:name", NameError::InvalidCharacters),
            ("émile", NameError::InvalidCharacters),
            (".dot", NameError::BadEdge),
            ("dash-", NameError::BadEdge),
            ("12345", NameError::AllDigits),
            ("Admin", NameError::Reserved),
            ("ME", NameError::Reserved),
        ];
        for (name, expected) in cases {
            assert_eq!(UserName::parse(name), Err(expected), "{name:?}");
        }
    }

    #[test]
    fn password_length_counts_characters() {
        assert_eq!(check_password("short"), Err(PasswordError::TooShort));
        assert_eq!(check_password("long enough"), Ok(()));
        // 8 characters, 16 bytes.
        assert_eq!(check_password("ééééééé\u{e9}"), Ok(()));
        assert_eq!(
            check_password(&"x".repeat(257)),
            Err(PasswordError::TooLong)
        );
    }

    #[test]
    fn email_check_is_loose_but_sane() {
        for ok in ["a@b.co", "first.last+tag@example.org"] {
            assert_eq!(check_email(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "no-at.example",
            "@example.com",
            "a@localhost",
            "a@.com",
            "a b@c.com",
        ] {
            assert_eq!(check_email(bad), Err(EmailError), "{bad}");
        }
    }

    #[test]
    fn hash_and_verify() {
        let hash = hash_password("correct horse");
        assert!(hash.starts_with("$argon2id$"));
        assert_eq!(
            verify_password("correct horse", &hash),
            Verification::Valid {
                needs_rehash: false
            }
        );
        assert_eq!(verify_password("wrong horse", &hash), Verification::Invalid);
        assert_eq!(
            verify_password("correct horse", "not a hash"),
            Verification::Invalid
        );
    }

    #[test]
    fn flags_outdated_parameters_for_rehash() {
        let weak = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(8 * 1024, 1, 1, None).unwrap(),
        );
        let hash = weak.hash_password(b"correct horse").unwrap().to_string();
        assert_eq!(
            verify_password("correct horse", &hash),
            Verification::Valid { needs_rehash: true }
        );
    }
}
