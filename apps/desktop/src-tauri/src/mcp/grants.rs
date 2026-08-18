//! Grant tokens: generation, digesting, and comparison.
//!
//! A token is 256 bits of `OsRng` shown to the user once and never persisted in
//! recoverable form. Only its SHA-256 is stored, and a presented token is
//! looked up *by* that digest, so a wrong token costs one indexed miss and
//! never iterates grants.
//!
//! Because the token is high-entropy random rather than user-chosen, a plain
//! digest is enough — there is no dictionary to run against it, so the password
//! hashing the keystore uses would buy nothing here.

use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Marks a token as ours in an agent's config file, where it sits beside tokens
/// for other services.
const TOKEN_PREFIX: &str = "luma_mcp_";
const TOKEN_BYTES: usize = 32;
/// `luma_mcp_` plus four characters: enough to tell two grants apart in the UI,
/// far too little to help anyone guess the rest.
const DISPLAY_PREFIX_CHARS: usize = TOKEN_PREFIX.len() + 4;

pub(crate) struct GeneratedToken {
    /// Shown once, then discarded.
    pub token: String,
    pub sha256: [u8; 32],
    pub prefix: String,
}

pub(crate) fn generate() -> GeneratedToken {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let token = format!(
        "{TOKEN_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    );
    let sha256 = digest(&token);
    let prefix = token.chars().take(DISPLAY_PREFIX_CHARS).collect();
    GeneratedToken {
        token,
        sha256,
        prefix,
    }
}

pub(crate) fn digest(token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

/// Compare two digests without an early return.
///
/// The digest lookup already avoids scanning grants, so this guards only the
/// final confirmation — but a length-independent compare costs nothing and
/// removes the question entirely.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Pull the bearer token out of an `Authorization` header value.
pub(crate) fn bearer_token(header: &str) -> Option<&str> {
    let (scheme, value) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_prefixed_and_self_consistent() {
        let first = generate();
        let second = generate();
        assert_ne!(first.token, second.token);
        assert!(first.token.starts_with(TOKEN_PREFIX));
        assert_eq!(first.prefix.len(), DISPLAY_PREFIX_CHARS);
        assert!(first.token.starts_with(&first.prefix));
        assert_eq!(digest(&first.token), first.sha256);
        assert_ne!(first.sha256, second.sha256);
    }

    #[test]
    fn the_prefix_reveals_nothing_useful() {
        let generated = generate();
        // 32 random bytes are 43 base64url characters; the prefix exposes four.
        assert!(generated.token.len() - generated.prefix.len() >= 39);
    }

    #[test]
    fn constant_time_eq_matches_equality() {
        let token = generate();
        assert!(constant_time_eq(&token.sha256, &digest(&token.token)));
        assert!(!constant_time_eq(&token.sha256, &digest("luma_mcp_other")));
        assert!(!constant_time_eq(&[1, 2, 3], &[1, 2]));
        assert!(constant_time_eq(&[], &[]));
    }

    #[test]
    fn a_single_bit_difference_is_rejected() {
        let mut left = [0u8; 32];
        let mut right = [0u8; 32];
        right[31] = 1;
        assert!(!constant_time_eq(&left, &right));
        left[31] = 1;
        assert!(constant_time_eq(&left, &right));
    }

    #[test]
    fn parses_bearer_headers_case_insensitively() {
        assert_eq!(bearer_token("Bearer abc"), Some("abc"));
        assert_eq!(bearer_token("bearer abc"), Some("abc"));
        assert_eq!(bearer_token("BEARER  abc  "), Some("abc"));
        assert_eq!(bearer_token("Basic abc"), None);
        assert_eq!(bearer_token("abc"), None);
        assert_eq!(bearer_token("Bearer "), None);
    }
}
