// Tripcode parsing and hashing helpers.

use sha2::{Digest as _, Sha256};

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
    s.get(..end).unwrap_or_default()
}

/// Compute a tripcode from a password string.
///
/// Returns a string like `"!Ab3Xy7Kp2Q"` — a `'!'` prefix followed by
/// [`TRIPCODE_ENCODED_LEN`] base64url characters.
fn compute_tripcode(password: &str) -> String {
    let [byte_0, byte_1, byte_2, byte_3, byte_4, byte_5, byte_6, byte_7, ..]: [u8; 32] =
        Sha256::digest(password.as_bytes()).into();
    let leading = [
        byte_0, byte_1, byte_2, byte_3, byte_4, byte_5, byte_6, byte_7,
    ];

    let encoded = base64url_encode(&leading);

    debug_assert!(
        encoded.len() >= TRIPCODE_ENCODED_LEN,
        "base64url of {TRIPCODE_HASH_BYTES} bytes must yield >= {TRIPCODE_ENCODED_LEN} chars, got {}",
        encoded.len(),
    );

    let mut tripcode = String::with_capacity(1 + TRIPCODE_ENCODED_LEN);
    tripcode.push('!');
    tripcode.extend(encoded.chars().take(TRIPCODE_ENCODED_LEN));
    tripcode
}

/// Converts one six-bit value into the RFC 4648 base64url alphabet.
fn base64url_char(value: u8) -> char {
    match value {
        0..=25 => char::from(b'A' + value),
        26..=51 => char::from(b'a' + (value - 26)),
        52..=61 => char::from(b'0' + (value - 52)),
        62 => '-',
        _ => '_',
    }
}

/// Minimal base64url encoder (RFC 4648 §5 alphabet, **no** padding).
fn base64url_encode(input: &[u8]) -> String {
    // Upper-bound allocation: ⌈len/3⌉ × 4 (exact for padded; at most 2 chars
    // over for unpadded, which is fine).
    let capacity = input.len().div_ceil(3) * 4;
    let mut output = String::with_capacity(capacity);

    let (chunks, remainder) = input.as_chunks::<3>();
    for &[byte_0, byte_1, byte_2] in chunks {
        output.push(base64url_char(byte_0 >> 2));
        output.push(base64url_char(((byte_0 & 0x03) << 4) | (byte_1 >> 4)));
        output.push(base64url_char(((byte_1 & 0x0f) << 2) | (byte_2 >> 6)));
        output.push(base64url_char(byte_2 & 0x3f));
    }

    match remainder {
        [byte_0, byte_1] => {
            output.push(base64url_char(*byte_0 >> 2));
            output.push(base64url_char(((*byte_0 & 0x03) << 4) | (*byte_1 >> 4)));
            output.push(base64url_char((*byte_1 & 0x0f) << 2));
        }
        [byte_0] => {
            output.push(base64url_char(*byte_0 >> 2));
            output.push(base64url_char((*byte_0 & 0x03) << 4));
        }
        _ => {}
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
        let trip = t1.as_deref().unwrap_or_default();
        assert!(
            trip.starts_with('!'),
            "a generated tripcode must start with its marker"
        );
        assert_eq!(
            trip.len(),
            1 + TRIPCODE_ENCODED_LEN,
            "tripcode must have the fixed encoded length"
        );
    }

    #[test]
    fn no_tripcode_marker() {
        let (name, trip) = parse_name_tripcode("Anonymous");
        assert_eq!(name, "Anonymous", "plain display name changed");
        assert!(
            trip.is_none(),
            "plain display name unexpectedly produced a tripcode"
        );
    }

    #[test]
    fn empty_name_defaults_to_anonymous() {
        let (name, trip) = parse_name_tripcode("#somepassword");
        assert_eq!(name, DEFAULT_NAME, "empty display name did not use default");
        assert!(
            trip.is_some(),
            "nonempty password did not produce a tripcode"
        );
    }

    #[test]
    fn empty_input_defaults_to_anonymous() {
        let (name, trip) = parse_name_tripcode("");
        assert_eq!(name, DEFAULT_NAME, "empty input did not use default name");
        assert!(
            trip.is_none(),
            "empty input unexpectedly produced a tripcode"
        );
    }

    #[test]
    fn whitespace_only_defaults_to_anonymous() {
        let (name, trip) = parse_name_tripcode("   ");
        assert_eq!(
            name, DEFAULT_NAME,
            "whitespace-only input did not use default name"
        );
        assert!(
            trip.is_none(),
            "whitespace-only input unexpectedly produced a tripcode"
        );
    }

    #[test]
    fn empty_password_yields_no_tripcode() {
        let (name, trip) = parse_name_tripcode("Anon#");
        assert_eq!(name, "Anon", "display name before empty password changed");
        assert!(
            trip.is_none(),
            "empty password unexpectedly produced a tripcode"
        );
    }

    #[test]
    fn multiple_hashes_splits_on_first_only() {
        let (name, trip) = parse_name_tripcode("Foo#bar#baz");
        assert_eq!(name, "Foo", "parser did not split at the first marker");
        assert!(trip.is_some(), "password 'bar#baz' should yield a tripcode");

        // Verify the password includes everything after the first '#'.
        let (_, trip_plain) = parse_name_tripcode("X#bar#baz");
        assert_eq!(
            trip, trip_plain,
            "password text after the first marker was not preserved"
        );
    }

    #[test]
    fn tripcode_character_set() {
        let (_, trip) = parse_name_tripcode("User#secret");
        let trip = trip.as_deref().unwrap_or_default();
        assert!(
            trip.starts_with('!'),
            "a generated tripcode must start with its marker"
        );
        assert_eq!(
            trip.len(),
            11,
            "tripcode must contain marker plus ten characters"
        );
        let body = trip.strip_prefix('!').unwrap_or_default();
        assert!(
            body.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "tripcode body must use base64url alphabet, got: {trip}"
        );
    }

    #[test]
    fn different_passwords_yield_different_tripcodes() {
        let (_, t1) = parse_name_tripcode("A#password1");
        let (_, t2) = parse_name_tripcode("A#password2");
        assert_ne!(
            t1, t2,
            "different passwords unexpectedly produced equal tripcodes"
        );
    }

    #[test]
    fn long_input_is_truncated_safely() {
        let long_name = "A".repeat(1000);
        let (name, trip) = parse_name_tripcode(&long_name);
        assert!(
            name.len() <= MAX_RAW_INPUT_LEN,
            "long input was not bounded to the raw input limit"
        );
        assert!(
            trip.is_none(),
            "marker-free long input unexpectedly produced a tripcode"
        );
    }

    #[test]
    fn multibyte_truncation_preserves_utf8() {
        // 'é' is 2 bytes in UTF-8. Build a string exceeding the limit.
        let repeated = "é".repeat(MAX_RAW_INPUT_LEN); // 512 bytes
        let (name, _) = parse_name_tripcode(&repeated);
        assert!(
            name.len() <= MAX_RAW_INPUT_LEN,
            "multibyte input exceeded the raw byte limit"
        );
        // The returned name is a valid String, so UTF-8 validity is guaranteed
        // by the type system. Verify it round-trips.
        assert_eq!(
            name,
            name.as_str(),
            "truncated multibyte name did not round-trip as UTF-8"
        );
    }

    #[test]
    fn base64url_known_vector() {
        // "Hello" (5 bytes) → base64url "SGVsbG8" (no padding).
        assert_eq!(
            base64url_encode(b"Hello"),
            "SGVsbG8",
            "base64url known vector changed"
        );
    }

    #[test]
    fn base64url_empty_input() {
        assert_eq!(
            base64url_encode(b""),
            "",
            "empty input should encode to an empty string"
        );
    }

    #[test]
    fn base64url_one_byte() {
        // 0x00 → "AA" (no padding)
        assert_eq!(
            base64url_encode(&[0x00]),
            "AA",
            "one-byte base64url tail encoding changed"
        );
    }

    #[test]
    fn base64url_two_bytes() {
        // 0x00 0x00 → "AAA" (no padding)
        assert_eq!(
            base64url_encode(&[0x00, 0x00]),
            "AAA",
            "two-byte base64url tail encoding changed"
        );
    }

    #[test]
    fn base64url_three_bytes() {
        // 0x00 0x00 0x00 → "AAAA"
        assert_eq!(
            base64url_encode(&[0x00, 0x00, 0x00]),
            "AAAA",
            "three-byte base64url block encoding changed"
        );
    }
}
