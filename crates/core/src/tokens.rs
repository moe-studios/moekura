//! Random secrets handed to clients (session tokens, invite codes, API
//! keys).
//!
//! Only a SHA-256 of each token is stored, so a database leak doesn't hand
//! out working credentials. The tokens carry 256 random bits, so a fast
//! unsalted hash is enough; slow hashing is only needed for passwords.

use sha2::{Digest, Sha256};

pub type TokenHash = [u8; 32];

/// A freshly generated token: `token` goes to the client, `hash` to the
/// database.
pub struct NewToken {
    pub token: String,
    pub hash: TokenHash,
}

impl NewToken {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random number generator is available");
        let token = hex::encode(bytes);
        let hash = hash_token(&token);
        Self { token, hash }
    }
}

/// What every API key starts with, so secret scanners can recognise one
/// that leaked.
pub const API_KEY_PREFIX: &str = "uwu_";

impl NewToken {
    /// A token for an API key: [`API_KEY_PREFIX`] and 64 hex digits.
    pub fn api_key() -> Self {
        let token = format!("{API_KEY_PREFIX}{}", Self::generate().token);
        let hash = hash_token(&token);
        Self { token, hash }
    }
}

pub fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_hash_consistently() {
        let a = NewToken::generate();
        let b = NewToken::generate();
        assert_ne!(a.token, b.token);
        assert_eq!(a.token.len(), 64);
        assert_eq!(hash_token(&a.token), a.hash);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn api_keys_are_recognisable() {
        let key = NewToken::api_key();
        assert!(key.token.starts_with("uwu_"));
        assert_eq!(key.token.len(), 4 + 64);
        assert_eq!(hash_token(&key.token), key.hash);
    }
}
