// utils/crypto.rs
//
// Security primitives:
//
// • Argon2id for admin password hashing — memory-hard, GPU-resistant.
//   Conservative parameters: t=2, m=65536, p=2.
//   This costs ~65 MiB RAM and ~200 ms per hash — acceptable for admin login,
//   makes brute-force attacks impractical even on purpose-built hardware.
//
// • SHA-256 for IP hashing — one-way transform. We never store raw IPs.
//   A salt (the cookie secret) is prepended so the hash can't be reversed
//   via precomputed tables even if the DB is leaked.
//
// • CSRF tokens — 32-byte random value encoded as hex, stored in a signed
//   cookie. Forms include it as a hidden field; handler verifies cookie == form.
//   Using signed cookies means the token can't be forged without the secret key.
//
// • Session IDs — 32-byte random value encoded as hex. Stored in DB with
//   expiry. HTTPOnly + SameSite=Strict cookie — not accessible from JS.
//
// • Deletion tokens — 16-byte random value encoded as hex. Stored in DB.
//   Posted as hidden form field at post time; user must supply to delete.
//
// FIX[MEDIUM-9]: All random token generation now uses OsRng directly.
// While rand::thread_rng() is cryptographically secure in rand 0.8 (ChaCha12
// seeded by OsRng), using OsRng explicitly makes the security property
// immediately visible to auditors without requiring knowledge of rand internals.

use anyhow::Result;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Algorithm, Argon2, Params, Version,
};
// rand_core::RngCore is the same trait instance as argon2's re-exported
// rand_core::OsRng implements — they share the same rand_core 0.6 crate.
use dashmap::DashMap;
use once_cell::sync::Lazy;
use rand_core::RngCore;
use sha2::{Digest, Sha256};

/// Hash an admin password using Argon2id.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    // t_cost=2, m_cost=64 MiB, p_cost=2 — conservative, works on any server.
    // FIX[LOW-7]: Removed hardware-specific comment.
    let params = Params::new(65536, 2, 2, None)
        .map_err(|e| anyhow::anyhow!("Argon2 params error: {}", e))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))?
        .to_string();
    Ok(hash)
}

/// Verify a password against an Argon2 hash.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed =
        PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("Invalid password hash: {}", e))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Generate a cryptographically secure random hex string of `bytes` length.
///
/// FIX[MEDIUM-9]: Uses OsRng directly (the OS CSPRNG) rather than thread_rng(),
/// making the security property explicit.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

/// Generate a session ID (32 random bytes → 64 hex chars).
pub fn new_session_id() -> String {
    random_hex(32)
}

/// Generate a deletion token (16 random bytes → 32 hex chars).
pub fn new_deletion_token() -> String {
    random_hex(16)
}

/// Generate a CSRF token (32 random bytes → 64 hex chars).
pub fn new_csrf_token() -> String {
    random_hex(32)
}

/// Hash an IP address with a secret salt. Output is a 64-char hex string.
/// The salt prevents rainbow-table attacks if the DB is leaked.
pub fn hash_ip(ip: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(ip.as_bytes());
    hex::encode(hasher.finalize())
}

/// Compute the SHA-256 of arbitrary bytes, returned as lowercase hex.
///
/// FIX[LOW-8]: Deduplicated from board.rs and thread.rs. Handlers should
/// call this instead of defining their own local sha256_hex function.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

// ─── PoW CAPTCHA (hashcash-style) ────────────────────────────────────────────
// The challenge is "{board_short}:{unix_minutes}" — valid for a 5-minute window.
// The client finds a nonce such that SHA-256(challenge + ":" + nonce) has at
// least POW_DIFFICULTY leading zero bits.

pub const POW_DIFFICULTY: u32 = 20; // ~1M avg iterations; ~50–200 ms in JS

/// FIX[NEW-C2]: In-memory nonce replay cache.
///
/// Maps "board_short:nonce" → unix timestamp (seconds) at which the nonce was
/// first accepted.  Any nonce seen within the PoW validity window (5 minutes)
/// is rejected on a second submission, preventing a solved nonce from being
/// replayed unlimited times.
///
/// Entries older than POW_WINDOW_SECS are pruned on every call to verify_pow
/// so memory usage is bounded by the rate of legitimate solves.
static SEEN_NONCES: Lazy<DashMap<String, i64>> = Lazy::new(DashMap::new);

/// The PoW grace window in seconds — must match the 5 × 60 s window in verify_pow.
const POW_WINDOW_SECS: i64 = 300;

/// Build the expected challenge string for the given board and time.
pub fn pow_challenge(board_short: &str, unix_ts: i64) -> String {
    let minute = unix_ts / 60;
    format!("{}:{}", board_short, minute)
}

/// Verify a submitted PoW nonce.  Accepts solutions for the current minute and
/// up to 4 prior minutes (5-minute grace window covering clock skew + solve time).
///
/// FIX[NEW-C2]: Each (board, nonce) pair is recorded in SEEN_NONCES after its
/// first successful verification and rejected on any subsequent call within the
/// same window, closing the replay attack vector.
pub fn verify_pow(board_short: &str, nonce: &str) -> bool {
    use sha2::{Digest, Sha256};
    let now = chrono::Utc::now().timestamp();
    let now_minutes = now / 60;

    // Prune stale entries to bound memory usage.
    SEEN_NONCES.retain(|_, ts| now - *ts < POW_WINDOW_SECS);

    // Check whether this (board, nonce) pair has already been accepted.
    let cache_key = format!("{}:{}", board_short, nonce);
    if SEEN_NONCES.contains_key(&cache_key) {
        return false; // FIX[NEW-C2]: replay rejected
    }

    // Try current minute and the 4 prior minutes.
    for delta in 0i64..=4 {
        let challenge = pow_challenge(board_short, (now_minutes - delta) * 60);
        let input = format!("{}:{}", challenge, nonce);
        let hash = Sha256::digest(input.as_bytes());
        if leading_zero_bits(&hash) >= POW_DIFFICULTY {
            // Record this nonce as consumed.
            SEEN_NONCES.insert(cache_key, now);
            return true;
        }
    }
    false
}

fn leading_zero_bits(bytes: &[u8]) -> u32 {
    let mut count = 0u32;
    for &byte in bytes {
        if byte == 0 {
            count += 8;
        } else {
            count += byte.leading_zeros();
            break;
        }
    }
    count
}
