// utils/crypto.rs
//
// Security primitives:
//
// • Argon2id for admin password hashing — memory-hard, GPU-resistant.
//   Parameters: t=2, m=65536 (64 MiB), p=2.
//   ~200 ms per hash — acceptable for admin login, impractical to brute-force.
//
// • SHA-256 for IP hashing — one-way transform. We never store raw IPs.
//   A salt (the cookie secret) is prepended so the hash can't be reversed
//   via precomputed tables even if the DB is leaked.
//
// • CSRF tokens — 32-byte random value encoded as hex, stored in a signed
//   cookie. Forms include it as a hidden field; handler verifies cookie == form.
//
// • Session IDs — 32-byte random value encoded as hex. Stored in DB with
//   expiry. HTTPOnly + SameSite=Strict cookie.
//
// • Deletion tokens — 16-byte random value encoded as hex. Stored in DB.
//
// All random token generation uses OsRng directly (OS CSPRNG), making the
// security property immediately visible to auditors.

use anyhow::Result;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
use dashmap::DashMap;
use rand_core::RngCore;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Maximum allowed nonce length in characters.
/// A legitimate nonce is a numeric string from the JS solver; anything longer
/// than this is adversarial or malformed and is rejected before allocation.
const MAX_NONCE_LEN: usize = 128;

/// Maximum allowed board short-name length in characters.
const MAX_BOARD_SHORT_LEN: usize = 32;

/// Hash an admin password using Argon2id.
///
/// Parameters: `t_cost=2`, `m_cost=64 MiB`, `p_cost=2`.
///
/// # Errors
/// Returns an error if Argon2 parameter construction or hashing fails.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let params =
        Params::new(65536, 2, 2, None).map_err(|e| anyhow::anyhow!("Argon2 params error: {e}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {e}"))?
        .to_string();
    Ok(hash)
}

/// Verify a password against an Argon2id hash (PHC string format).
///
/// Returns `Ok(true)` on match, `Ok(false)` on mismatch.
///
/// # Errors
/// Returns an error if the stored hash string is malformed.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed =
        PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("Invalid password hash: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Generate a cryptographically secure random hex string.
///
/// `bytes` is the number of random bytes; the returned string is `2 * bytes`
/// hex characters long. Uses [`OsRng`] directly for explicit CSPRNG provenance.
#[must_use]
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Generate a session ID (32 random bytes → 64 hex chars).
#[must_use]
#[inline]
pub fn new_session_id() -> String {
    random_hex(32)
}

/// Generate a deletion token (16 random bytes → 32 hex chars).
#[must_use]
#[inline]
pub fn new_deletion_token() -> String {
    random_hex(16)
}

/// Generate a CSRF token (32 random bytes → 64 hex chars).
#[must_use]
#[inline]
pub fn new_csrf_token() -> String {
    random_hex(32)
}

/// Hash an IP address with a secret salt. Output is a 64-char hex string.
///
/// The salt prevents rainbow-table attacks if the DB is leaked.
/// A `:` separator is placed between salt and IP to prevent ambiguity when
/// one value is a prefix of another.
#[must_use]
pub fn hash_ip(ip: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(ip.as_bytes());
    hex::encode(hasher.finalize())
}

/// Compute the SHA-256 of arbitrary bytes, returned as lowercase hex.
///
/// Deduplicated helper — all handlers should call this rather than defining
/// their own local `sha256_hex` function.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

// ─── PoW CAPTCHA (hashcash-style) ────────────────────────────────────────────
// The challenge is "{board_short}:{unix_minutes}" — valid for a 5-minute window.
// The client finds a nonce such that SHA-256(challenge + ":" + nonce) has at
// least POW_DIFFICULTY leading zero bits.

/// Number of leading zero bits required for a valid `PoW` solution.
/// ~1 M average iterations; ~50–200 ms in browser JS.
pub const POW_DIFFICULTY: u32 = 20;

/// In-memory nonce replay cache.
///
/// Maps `"board_short:nonce"` → unix timestamp (seconds) of first acceptance.
/// Entries older than [`POW_WINDOW_SECS`] are pruned on every call to
/// [`verify_pow`] so memory usage is bounded by the rate of legitimate solves.
static SEEN_NONCES: LazyLock<DashMap<String, i64>> = LazyLock::new(DashMap::new);

/// `PoW` validity window in seconds (5 minutes).
const POW_WINDOW_SECS: i64 = 300;

/// Number of past minutes (inclusive of current) to accept `PoW` solutions for.
const POW_GRACE_MINUTES: i64 = 5;

/// Build the expected challenge string for the given board and time.
#[must_use]
pub fn pow_challenge(board_short: &str, unix_ts: i64) -> String {
    let minute = unix_ts / 60;
    format!("{board_short}:{minute}")
}

/// Returns `true` if every byte in `s` is ASCII alphanumeric, `_`, or `-`.
#[inline]
fn is_valid_board_short(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_BOARD_SHORT_LEN
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Returns `true` if `s` is a non-empty, bounded, ASCII-alphanumeric nonce.
#[inline]
fn is_valid_nonce(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NONCE_LEN && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Verify a submitted `PoW` nonce.
///
/// Accepts solutions for the current minute and up to
/// [`POW_GRACE_MINUTES`] − 1 prior minutes (grace window covering clock skew
/// and solve time).
///
/// Each `(board, nonce)` pair is **atomically** claimed on first successful
/// verification via the [`DashMap::entry`] API — the shard lock is held for
/// the duration of the check-and-insert, preventing TOCTOU replay races even
/// under concurrent load.
#[must_use]
pub fn verify_pow(board_short: &str, nonce: &str) -> bool {
    use dashmap::mapref::entry::Entry;

    // ── Input validation ──────────────────────────────────────────────
    // Reject empty, oversized, or non-ASCII inputs before any allocation
    // or hashing work to prevent abuse and cache-key ambiguity.
    if !is_valid_board_short(board_short) || !is_valid_nonce(nonce) {
        return false;
    }

    let now = chrono::Utc::now().timestamp();
    let now_minutes = now / 60;

    // Prune stale entries to bound memory usage.
    #[allow(clippy::arithmetic_side_effects)]
    SEEN_NONCES.retain(|_, ts| now - *ts < POW_WINDOW_SECS);

    // Try current minute and the prior (POW_GRACE_MINUTES - 1) minutes.
    let cache_key = format!("{board_short}:{nonce}");
    for delta in 0..POW_GRACE_MINUTES {
        #[allow(clippy::arithmetic_side_effects)]
        let challenge = pow_challenge(board_short, (now_minutes - delta) * 60);
        let input = format!("{challenge}:{nonce}");
        let hash = Sha256::digest(input.as_bytes());

        if leading_zero_bits(&hash) >= POW_DIFFICULTY {
            // Atomically claim this nonce. entry() holds the shard lock
            // for the duration, so no concurrent request can slip between
            // the existence check and the insertion.
            match SEEN_NONCES.entry(cache_key) {
                Entry::Vacant(e) => {
                    e.insert(now);
                    return true;
                }
                Entry::Occupied(_) => {
                    return false; // already consumed — replay rejected
                }
            }
        }
    }
    false
}

/// Count the number of leading zero bits in a byte slice.
#[inline]
fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut count = 0u32;
    for &byte in bytes {
        let lz = byte.leading_zeros();
        count = count.saturating_add(lz);
        if lz < 8 {
            break;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;

    // ── Password hashing ─────────────────────────────────────────────

    #[test]
    fn hash_and_verify_password() {
        let hash = hash_password("correct-horse-battery-staple").expect("hash_password failed");
        assert!(verify_password("correct-horse-battery-staple", &hash).expect("verify failed"));
        assert!(!verify_password("wrong-password", &hash).expect("verify failed"));
    }

    #[test]
    fn verify_password_rejects_malformed_hash() {
        assert!(verify_password("anything", "not-a-phc-string").is_err());
    }

    // ── Random hex ───────────────────────────────────────────────────

    #[test]
    fn random_hex_length() {
        assert_eq!(random_hex(16).len(), 32);
        assert_eq!(random_hex(32).len(), 64);
    }

    #[test]
    fn random_hex_is_valid_hex() {
        let h = random_hex(32);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn random_hex_is_not_constant() {
        // Vanishingly unlikely to collide for 32 bytes.
        assert_ne!(random_hex(32), random_hex(32));
    }

    // ── Token generators ─────────────────────────────────────────────

    #[test]
    fn session_id_length() {
        assert_eq!(new_session_id().len(), 64);
    }

    #[test]
    fn deletion_token_length() {
        assert_eq!(new_deletion_token().len(), 32);
    }

    #[test]
    fn csrf_token_length() {
        assert_eq!(new_csrf_token().len(), 64);
    }

    // ── IP hashing ───────────────────────────────────────────────────

    #[test]
    fn hash_ip_deterministic() {
        let a = hash_ip("127.0.0.1", "secret");
        let b = hash_ip("127.0.0.1", "secret");
        assert_eq!(a, b);
    }

    #[test]
    fn hash_ip_different_salt_differs() {
        let a = hash_ip("127.0.0.1", "salt-a");
        let b = hash_ip("127.0.0.1", "salt-b");
        assert_ne!(a, b);
    }

    #[test]
    fn hash_ip_different_ip_differs() {
        let a = hash_ip("10.0.0.1", "salt");
        let b = hash_ip("10.0.0.2", "salt");
        assert_ne!(a, b);
    }

    #[test]
    fn hash_ip_length() {
        assert_eq!(hash_ip("::1", "s").len(), 64); // SHA-256 → 32 bytes → 64 hex
    }

    // ── sha256_hex ───────────────────────────────────────────────────

    #[test]
    fn sha256_hex_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // ── leading_zero_bits ────────────────────────────────────────────

    #[test]
    fn leading_zeros_all_zero() {
        assert_eq!(leading_zero_bits(&[0, 0, 0, 0]), 32);
    }

    #[test]
    fn leading_zeros_first_byte_nonzero() {
        assert_eq!(leading_zero_bits(&[0x0F, 0xFF]), 4);
        assert_eq!(leading_zero_bits(&[0x80, 0x00]), 0);
        assert_eq!(leading_zero_bits(&[0x01, 0x00]), 7);
    }

    #[test]
    fn leading_zeros_second_byte() {
        assert_eq!(leading_zero_bits(&[0x00, 0x01]), 15); // 8 + 7
    }

    #[test]
    fn leading_zeros_empty() {
        assert_eq!(leading_zero_bits(&[]), 0);
    }

    // ── PoW challenge format ─────────────────────────────────────────

    #[test]
    fn pow_challenge_format() {
        let c = pow_challenge("b", 120); // 120 seconds = minute 2
        assert_eq!(c, "b:2");
    }

    // ── Input validation in verify_pow ───────────────────────────────

    #[test]
    fn verify_pow_rejects_empty_board() {
        assert!(!verify_pow("", "12345"));
    }

    #[test]
    fn verify_pow_rejects_empty_nonce() {
        assert!(!verify_pow("b", ""));
    }

    #[test]
    fn verify_pow_rejects_oversized_nonce() {
        let long = "a".repeat(MAX_NONCE_LEN + 1);
        assert!(!verify_pow("b", &long));
    }

    #[test]
    fn verify_pow_rejects_non_ascii_nonce() {
        assert!(!verify_pow("b", "nonce\x00evil"));
        assert!(!verify_pow("b", "nönce"));
    }

    #[test]
    fn verify_pow_rejects_non_ascii_board() {
        assert!(!verify_pow("böard", "12345"));
        assert!(!verify_pow("a:b", "12345")); // colon not allowed
    }
}
