//! Time-based one-time passwords (RFC 6238) for two-factor login: the
//! six-digit codes authenticator apps show, and one-time recovery codes
//! for when the app is lost.

use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

use crate::tokens::{TokenHash, hash_token};

/// Seconds each code is valid for, as every authenticator app assumes.
pub const STEP_SECS: i64 = 30;
const DIGITS: u32 = 6;
/// 160 bits, the size RFC 4226 recommends for HMAC-SHA1.
const SECRET_LEN: usize = 20;
/// Recovery codes given out at once.
pub const RECOVERY_CODES: usize = 10;

/// The shared secret behind a user's codes.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Secret {
    pub fn generate() -> Self {
        let mut bytes = vec![0u8; SECRET_LEN];
        getrandom::fill(&mut bytes).expect("the OS random number generator is available");
        Self(bytes)
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Base32, how apps take a secret typed in by hand.
    pub fn to_base32(&self) -> String {
        base32(&self.0)
    }

    /// The `otpauth://` URI a QR code carries, labelled with the site and
    /// the account name.
    pub fn uri(&self, issuer: &str, account: &str) -> String {
        let encode = |s: &str| {
            url::form_urlencoded::byte_serialize(s.as_bytes())
                .collect::<String>()
                .replace('+', "%20")
        };
        format!(
            "otpauth://totp/{issuer}:{account}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}",
            issuer = encode(issuer),
            account = encode(account),
            secret = self.to_base32(),
        )
    }

    /// The code for time step `step`.
    fn code(&self, step: i64) -> u32 {
        let mut mac = Hmac::<Sha1>::new_from_slice(&self.0).expect("HMAC takes keys of any length");
        mac.update(&step.to_be_bytes());
        let hash = mac.finalize().into_bytes();
        // Dynamic truncation, RFC 4226 section 5.3.
        let offset = usize::from(hash[hash.len() - 1] & 0x0f);
        let value = u32::from_be_bytes([
            hash[offset] & 0x7f,
            hash[offset + 1],
            hash[offset + 2],
            hash[offset + 3],
        ]);
        value % 10u32.pow(DIGITS)
    }

    /// The code an app shows at `unix_time`.
    pub fn code_at(&self, unix_time: i64) -> String {
        let step = unix_time.div_euclid(STEP_SECS);
        format!("{:0width$}", self.code(step), width = DIGITS as usize)
    }

    /// Checks `input` against the codes around `unix_time`, allowing a
    /// step either way for clock drift. Returns the step it matched, which
    /// must be later than `last_step` (the last one accepted), so each
    /// code works only once.
    pub fn verify(&self, input: &str, unix_time: i64, last_step: i64) -> Option<i64> {
        let input: String = input.chars().filter(|c| !c.is_whitespace()).collect();
        if input.len() != DIGITS as usize || !input.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let now = unix_time.div_euclid(STEP_SECS);
        (now - 1..=now + 1)
            .filter(|&step| step > last_step)
            .find(|&step| {
                let expected = self.code_at(step * STEP_SECS);
                constant_time_eq(expected.as_bytes(), input.as_bytes())
            })
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// RFC 4648 base32, without padding.
fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    for chunk in bytes.chunks(5) {
        let mut buffer = [0u8; 5];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let bits = buffer
            .iter()
            .fold(0u64, |acc, &b| (acc << 8) | u64::from(b));
        let chars = (chunk.len() * 8).div_ceil(5);
        for i in 0..chars {
            let index = (bits >> (35 - i * 5)) & 0x1f;
            out.push(char::from(ALPHABET[index as usize]));
        }
    }
    out
}

/// Fresh recovery codes, like `k3m9x-p2q7w`: what to show the user once.
pub fn recovery_codes() -> Vec<String> {
    // No 0/o, 1/l/i, since people copy these by hand.
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    // Bytes at or above this would favour the first few characters.
    const LIMIT: u8 = (256 / ALPHABET.len() * ALPHABET.len()) as u8;
    (0..RECOVERY_CODES)
        .map(|_| {
            let mut chars = String::with_capacity(10);
            while chars.len() < 10 {
                let mut bytes = [0u8; 16];
                getrandom::fill(&mut bytes).expect("the OS random number generator is available");
                chars.extend(
                    bytes
                        .iter()
                        .filter(|&&b| b < LIMIT)
                        .map(|&b| char::from(ALPHABET[usize::from(b) % ALPHABET.len()]))
                        .take(10 - chars.len()),
                );
            }
            format!("{}-{}", &chars[..5], &chars[5..])
        })
        .collect()
}

/// What's stored for a recovery code: a hash of it, ignoring case,
/// spaces and dashes.
pub fn hash_recovery_code(code: &str) -> TokenHash {
    let normalized: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .flat_map(char::to_lowercase)
        .collect();
    hash_token(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SHA-1 test vectors from RFC 6238, appendix B, at six digits.
    #[test]
    fn matches_the_rfc() {
        let secret = Secret::from_bytes(b"12345678901234567890".to_vec());
        for (time, code) in [
            (59, 287_082),
            (1_111_111_109, 81_804),
            (1_111_111_111, 50_471),
            (1_234_567_890, 5_924),
            (2_000_000_000, 279_037),
            (20_000_000_000, 353_130),
        ] {
            assert_eq!(secret.code(time / STEP_SECS), code, "at {time}");
        }
    }

    #[test]
    fn verifies_nearby_codes_once() {
        let secret = Secret::from_bytes(b"12345678901234567890".to_vec());
        let time = 1_234_567_890;
        let step = time / STEP_SECS;
        assert_eq!(secret.code_at(time), "005924");
        assert_eq!(secret.verify("005924", time, 0), Some(step));
        assert_eq!(secret.verify(" 005 924 ", time, 0), Some(step));
        // A step later still works; two steps don't.
        assert_eq!(secret.verify("005924", time + STEP_SECS, 0), Some(step));
        assert_eq!(secret.verify("005924", time + 2 * STEP_SECS, 0), None);
        // Once used, a code (or an older one) is refused.
        assert_eq!(secret.verify("005924", time, step), None);
        assert_eq!(secret.verify("5924", time, 0), None);
        assert_eq!(secret.verify("00592a", time, 0), None);
    }

    #[test]
    fn encodes_base32_and_uris() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        let secret = Secret::from_bytes(b"12345678901234567890".to_vec());
        assert_eq!(secret.to_base32(), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(
            secret.uri("My Booru", "alice"),
            "otpauth://totp/My%20Booru:alice?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ\
             &issuer=My%20Booru&algorithm=SHA1&digits=6&period=30"
        );
        assert_eq!(Secret::generate().as_bytes().len(), 20);
    }

    #[test]
    fn recovery_codes_are_unique_and_forgiving() {
        let codes = recovery_codes();
        assert_eq!(codes.len(), RECOVERY_CODES);
        let mut unique = codes.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), codes.len());
        assert!(
            codes
                .iter()
                .all(|c| c.len() == 11 && c.as_bytes()[5] == b'-')
        );
        assert_eq!(
            hash_recovery_code("ABCDE-fghjk"),
            hash_recovery_code(" abcde fghjk ")
        );
    }
}
