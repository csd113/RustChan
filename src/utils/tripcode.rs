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
        end = end.saturating_sub(1);
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
        0..=25 => char::from(b'A'.saturating_add(value)),
        26..=51 => char::from(b'a'.saturating_add(value.saturating_sub(26))),
        52..=61 => char::from(b'0'.saturating_add(value.saturating_sub(52))),
        62 => '-',
        _ => '_',
    }
}

/// Minimal base64url encoder (RFC 4648 §5 alphabet, **no** padding).
fn base64url_encode(input: &[u8]) -> String {
    // Upper-bound allocation: ⌈len/3⌉ × 4 (exact for padded; at most 2 chars
    // over for unpadded, which is fine).
    let capacity = input
        .len()
        .div_ceil(3)
        .checked_mul(4)
        .unwrap_or(input.len());
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
