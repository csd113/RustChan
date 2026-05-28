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
// All random token generation uses the OS CSPRNG directly, making the
// security property immediately visible to auditors.

use anyhow::Result;
use argon2::{
    password_hash::{
        rand_core::OsRng as PasswordOsRng, PasswordHash, PasswordHasher as _,
        PasswordVerifier as _, SaltString,
    },
    Algorithm, Argon2, Params, Version,
};
use getrandom::SysRng;
use rand_core::TryRng as _;
use sha2::{Digest as _, Sha256};
use std::io::Write as _;

/// Hash an admin password using Argon2id.
///
/// Parameters: `t_cost=2`, `m_cost=64 MiB`, `p_cost=2`.
///
/// # Errors
/// Returns an error if Argon2 parameter construction or hashing fails.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut PasswordOsRng);
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
#[expect(
    clippy::exit,
    reason = "fail-closed randomness failures must terminate before issuing weak tokens"
)]
fn fatal_randomness_error(context: &str, error: &impl std::fmt::Display) -> ! {
    let _ = writeln!(
        std::io::stderr().lock(),
        "Fatal: OS randomness unavailable while {context}: {error}"
    );
    std::process::exit(1);
}

pub fn fill_os_random_or_exit(bytes: &mut [u8], context: &str) {
    if let Err(error) = SysRng.try_fill_bytes(bytes) {
        fatal_randomness_error(context, &error);
    }
}

#[must_use]
pub fn os_random_u32_or_exit(context: &str) -> u32 {
    match SysRng.try_next_u32() {
        Ok(value) => value,
        Err(error) => fatal_randomness_error(context, &error),
    }
}

/// `bytes` is the number of random bytes; the returned string is `2 * bytes`
/// hex characters long. Uses the OS CSPRNG directly for explicit provenance.
#[must_use]
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    fill_os_random_or_exit(&mut buf, "generating a random hex token");
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

#[must_use]
pub fn sign_csrf_token(raw_token: &str, secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(b":csrf:");
    hasher.update(raw_token.as_bytes());
    hex::encode(hasher.finalize())
}

#[must_use]
pub fn sign_scoped_csrf_token(raw_token: &str, secret: &str, scope: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(b":csrf:");
    hasher.update(scope.as_bytes());
    hasher.update(b":");
    hasher.update(raw_token.as_bytes());
    hex::encode(hasher.finalize())
}

#[must_use]
pub fn make_csrf_form_token(raw_token: &str, secret: &str) -> String {
    format!("{raw_token}.{}", sign_csrf_token(raw_token, secret))
}

#[must_use]
pub fn make_scoped_csrf_form_token(raw_token: &str, secret: &str, scope: &str) -> String {
    format!(
        "{raw_token}.{}",
        sign_scoped_csrf_token(raw_token, secret, scope)
    )
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

// ─── Password validation ──────────────────────────────────────────────────────

/// Validate an admin password meets minimum requirements.
/// Minimum 8 characters (enforced here; tighten as needed).
///
/// # Errors
/// Returns an error if the password does not meet the minimum requirements.
pub fn validate_password(p: &str) -> anyhow::Result<()> {
    if p.len() < 8 {
        anyhow::bail!("Password must be at least 8 characters.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {

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
}
