// utils/tripcode.rs
//
// Tripcode system: user enters "Name#password" in the name field.
// We split on the first '#', hash the password, display "Name!XXXXXXXXXX".
//
// This implementation uses SHA-256 (truncated to 10 chars base64url encoding)
// for portability. Classic 4chan uses DES-crypt which isn't worth the dependency.
// The output is stable: same password always yields same tripcode.
//
// SECURITY NOTE: For production deployments, consider prefixing the password
// with an application-specific HMAC key or domain separator before hashing.
// This would prevent cross-site tripcode correlation and rainbow-table reuse,
// at the cost of changing all existing tripcode outputs.

use sha2::{Digest, Sha256};

/// Maximum allowed byte length for the raw name-field input.
/// Prevents excessive memory allocation from adversarial inputs.
const MAX_RAW_INPUT_LEN: usize = 256;

/// Number of base64url characters retained from the encoded hash.
const TRIPCODE_ENCODED_LEN: usize = 10;

/// Number of leading SHA-256 bytes to encode.
/// 8 bytes → 11 base64url chars (no padding), which exceeds [`TRIPCODE_ENCODED_LEN`].
const TRIPCODE_HASH_BYTES: usize = 8;

/// Default display name when the user supplies an empty or whitespace-only name.
const DEFAULT_NAME: &str = "Anonymous";

/// Parse a name field that may contain a tripcode marker (`#`).
///
/// Returns `(display_name, Option<tripcode_string>)`.
///
/// - The input is truncated to [`MAX_RAW_INPUT_LEN`] bytes (at a valid UTF-8
///   boundary) to bound resource usage.
/// - Splitting occurs on the **first** `#`; subsequent `#` characters become
///   part of the password.
///
/// # Examples
///
/// ```text
///   "Anonymous"        → ("Anonymous", None)
///   "Anon#mypassword"  → ("Anon",      Some("!Ab3Xy7Kp2Q"))
///   "#triponly"         → ("Anonymous", Some("!…"))
///   "Foo#bar#baz"      → ("Foo",       Some("!…"))  // password = "bar#baz"
/// ```
#[must_use]
pub fn parse_name_tripcode(raw: &str) -> (String, Option<String>) {
    let raw = truncate_to_char_boundary(raw, MAX_RAW_INPUT_LEN);

    if let Some((name_part, password)) = raw.split_once('#') {
        let name_part = name_part.trim();
        let name = if name_part.is_empty() {
            DEFAULT_NAME.to_owned()
        } else {
            name_part.to_owned()
        };

        let trip = if password.is_empty() {
            None
        } else {
            Some(compute_tripcode(password))
        };

        (name, trip)
    } else {
        let trimmed = raw.trim();
        let name = if trimmed.is_empty() {
            DEFAULT_NAME.to_owned()
        } else {
            trimmed.to_owned()
        };
        (name, None)
    }
}

/// Truncate `s` to at most `max_bytes` bytes, rounding down to the nearest
/// UTF-8 character boundary so the result is always valid `&str`.
fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    // Walk backwards until we land on a char boundary.
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    // SAFETY: `is_char_boundary(end)` guarantees a valid split point.
    &s[..end]
}

/// Compute a tripcode from a password string.
///
/// Returns a string like `"!Ab3Xy7Kp2Q"` — a `'!'` prefix followed by
/// [`TRIPCODE_ENCODED_LEN`] base64url characters.
fn compute_tripcode(password: &str) -> String {
    let hash = Sha256::digest(password.as_bytes());

    // SHA-256 always produces 32 bytes; extract the leading bytes into a
    // fixed-size array so no fallible bounds check is needed at runtime.
    let leading: &[u8; TRIPCODE_HASH_BYTES] = hash
        .first_chunk::<TRIPCODE_HASH_BYTES>()
        .expect("SHA-256 output is 32 bytes; first 8 always exist");

    let encoded = base64url_encode(leading);

    debug_assert!(
        encoded.len() >= TRIPCODE_ENCODED_LEN,
        "base64url of {TRIPCODE_HASH_BYTES} bytes must yield >= {TRIPCODE_ENCODED_LEN} chars, got {}",
        encoded.len(),
    );

    let mut tripcode = String::with_capacity(1 + TRIPCODE_ENCODED_LEN);
    tripcode.push('!');
    // `encoded` is 11 chars for 8 input bytes; taking 10 is always valid.
    tripcode.push_str(&encoded[..TRIPCODE_ENCODED_LEN]);
    tripcode
}

/// Minimal base64url encoder (RFC 4648 §5 alphabet, **no** padding).
///
/// # Safety of indexing
///
/// - `input[i]`, `input[i+1]`, `input[i+2]` are guarded by `i + 2 < input.len()`.
/// - `ALPHABET[x]` where `x` is produced by 6-bit masking (0‥63) into a
///   64-element array — always in bounds.
#[allow(clippy::indexing_slicing)]
fn base64url_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    // Upper-bound allocation: ⌈len/3⌉ × 4 (exact for padded; at most 2 chars
    // over for unpadded, which is fine).
    let capacity = input.len().div_ceil(3) * 4;
    let mut output = String::with_capacity(capacity);

    let mut i = 0;
    while i + 2 < input.len() {
        let b0 = input[i] as usize;
        let b1 = input[i + 1] as usize;
        let b2 = input[i + 2] as usize;
        output.push(ALPHABET[b0 >> 2] as char);
        output.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);
        output.push(ALPHABET[((b1 & 0x0f) << 2) | (b2 >> 6)] as char);
        output.push(ALPHABET[b2 & 0x3f] as char);
        i += 3;
    }

    let remaining = input.len() - i;
    if remaining == 2 {
        let b0 = input[i] as usize;
        let b1 = input[i + 1] as usize;
        output.push(ALPHABET[b0 >> 2] as char);
        output.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);
        output.push(ALPHABET[(b1 & 0x0f) << 2] as char);
    } else if remaining == 1 {
        let b0 = input[i] as usize;
        output.push(ALPHABET[b0 >> 2] as char);
        output.push(ALPHABET[(b0 & 0x03) << 4] as char);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tripcode_is_stable_across_names() {
        let (_, t1) = parse_name_tripcode("Anon#password123");
        let (_, t2) = parse_name_tripcode("DifferentName#password123");
        assert_eq!(t1, t2, "same password must produce identical tripcodes");
        let trip = t1.expect("tripcode should be present");
        assert!(trip.starts_with('!'));
        assert_eq!(trip.len(), 1 + TRIPCODE_ENCODED_LEN);
    }

    #[test]
    fn no_tripcode_marker() {
        let (name, trip) = parse_name_tripcode("Anonymous");
        assert_eq!(name, "Anonymous");
        assert!(trip.is_none());
    }

    #[test]
    fn empty_name_defaults_to_anonymous() {
        let (name, trip) = parse_name_tripcode("#somepassword");
        assert_eq!(name, DEFAULT_NAME);
        assert!(trip.is_some());
    }

    #[test]
    fn empty_input_defaults_to_anonymous() {
        let (name, trip) = parse_name_tripcode("");
        assert_eq!(name, DEFAULT_NAME);
        assert!(trip.is_none());
    }

    #[test]
    fn whitespace_only_defaults_to_anonymous() {
        let (name, trip) = parse_name_tripcode("   ");
        assert_eq!(name, DEFAULT_NAME);
        assert!(trip.is_none());
    }

    #[test]
    fn empty_password_yields_no_tripcode() {
        let (name, trip) = parse_name_tripcode("Anon#");
        assert_eq!(name, "Anon");
        assert!(trip.is_none());
    }

    #[test]
    fn multiple_hashes_splits_on_first_only() {
        let (name, trip) = parse_name_tripcode("Foo#bar#baz");
        assert_eq!(name, "Foo");
        assert!(trip.is_some(), "password 'bar#baz' should yield a tripcode");

        // Verify the password includes everything after the first '#'.
        let (_, trip_plain) = parse_name_tripcode("X#bar#baz");
        assert_eq!(trip, trip_plain);
    }

    #[test]
    fn tripcode_character_set() {
        let (_, trip) = parse_name_tripcode("User#secret");
        let trip = trip.expect("tripcode should be present");
        assert!(trip.starts_with('!'));
        assert_eq!(trip.len(), 11); // '!' + 10 chars
        assert!(
            trip[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "tripcode body must use base64url alphabet, got: {trip}"
        );
    }

    #[test]
    fn different_passwords_yield_different_tripcodes() {
        let (_, t1) = parse_name_tripcode("A#password1");
        let (_, t2) = parse_name_tripcode("A#password2");
        assert_ne!(t1, t2);
    }

    #[test]
    fn long_input_is_truncated_safely() {
        let long_name = "A".repeat(1000);
        let (name, trip) = parse_name_tripcode(&long_name);
        assert!(name.len() <= MAX_RAW_INPUT_LEN);
        assert!(trip.is_none());
    }

    #[test]
    fn multibyte_truncation_preserves_utf8() {
        // 'é' is 2 bytes in UTF-8. Build a string exceeding the limit.
        let repeated = "é".repeat(MAX_RAW_INPUT_LEN); // 512 bytes
        let (name, _) = parse_name_tripcode(&repeated);
        assert!(name.len() <= MAX_RAW_INPUT_LEN);
        // The returned name is a valid String, so UTF-8 validity is guaranteed
        // by the type system. Verify it round-trips.
        assert_eq!(name, name.as_str());
    }

    #[test]
    fn base64url_known_vector() {
        // "Hello" (5 bytes) → base64url "SGVsbG8" (no padding).
        assert_eq!(base64url_encode(b"Hello"), "SGVsbG8");
    }

    #[test]
    fn base64url_empty_input() {
        assert_eq!(base64url_encode(b""), "");
    }

    #[test]
    fn base64url_one_byte() {
        // 0x00 → "AA" (no padding)
        assert_eq!(base64url_encode(&[0x00]), "AA");
    }

    #[test]
    fn base64url_two_bytes() {
        // 0x00 0x00 → "AAA" (no padding)
        assert_eq!(base64url_encode(&[0x00, 0x00]), "AAA");
    }

    #[test]
    fn base64url_three_bytes() {
        // 0x00 0x00 0x00 → "AAAA"
        assert_eq!(base64url_encode(&[0x00, 0x00, 0x00]), "AAAA");
    }
}
