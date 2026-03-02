// utils/crypto.rs
//
// Security primitives:
//
// • Argon2id for admin password hashing — memory-hard, GPU-resistant.
//   We use conservative params suitable for a Pi 4 (t=2, m=65536, p=2).
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

use anyhow::Result;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, Params, Algorithm, Version,
};
use rand::Rng;
use sha2::{Digest, Sha256};

/// Hash an admin password using Argon2id.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    // t_cost=2, m_cost=64MiB, p_cost=2 — good Pi 4 balance
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
    let parsed = PasswordHash::new(hash)
        .map_err(|e| anyhow::anyhow!("Invalid password hash: {}", e))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Generate a cryptographically secure random hex string of `bytes` length.
pub fn random_hex(bytes: usize) -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..bytes).map(|_| rng.gen()).collect();
    hex::encode(bytes)
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
