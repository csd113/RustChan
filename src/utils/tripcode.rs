// utils/tripcode.rs
//
// Tripcode system: user enters "Name#password" in the name field.
// We split on the first '#', hash the password, display "Name!XXXXXXXXXX".
//
// This implementation uses SHA-256 (truncated to 10 chars base64-like encoding)
// for portability. Classic 4chan uses DES-crypt which isn't worth the dependency.
// The output is stable: same password always yields same tripcode.

use sha2::{Digest, Sha256};

/// Parse a name field that may contain a tripcode marker.
/// Returns (display_name, Option<tripcode_string>).
///
/// Examples:
///   "Anonymous"        → ("Anonymous", None)
///   "Anon#mypassword"  → ("Anon", Some("!Ab3Xy7Kp2Q"))
///   "#triponly"        → ("Anonymous", Some("!Ab3Xy7Kp2Q"))
pub fn parse_name_tripcode(raw: &str) -> (String, Option<String>) {
    if let Some(pos) = raw.find('#') {
        let name_part = raw[..pos].trim();
        let password = &raw[pos + 1..];

        let name = if name_part.is_empty() {
            "Anonymous".to_string()
        } else {
            name_part.to_string()
        };

        let trip = if password.is_empty() {
            None
        } else {
            Some(compute_tripcode(password))
        };

        (name, trip)
    } else {
        let name = if raw.trim().is_empty() {
            "Anonymous".to_string()
        } else {
            raw.trim().to_string()
        };
        (name, None)
    }
}

/// Compute a tripcode from a password string.
/// Returns a string like "!Ab3Xy7Kp2Q" (11 chars: '!' + 10 alphanum).
fn compute_tripcode(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    let result = hasher.finalize();
    // Encode first 7.5 bytes as 10 base64url chars (no padding)
    let encoded = base64url_encode(&result[..8]);
    format!("!{}", &encoded[..10])
}

/// Minimal base64url encoding (URL-safe alphabet, no padding)
fn base64url_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::new();
    let mut i = 0;
    while i + 2 < input.len() {
        let b0 = input[i] as usize;
        let b1 = input[i + 1] as usize;
        let b2 = input[i + 2] as usize;
        output.push(ALPHABET[b0 >> 2] as char);
        output.push(ALPHABET[((b0 & 3) << 4) | (b1 >> 4)] as char);
        output.push(ALPHABET[((b1 & 0xf) << 2) | (b2 >> 6)] as char);
        output.push(ALPHABET[b2 & 0x3f] as char);
        i += 3;
    }
    if i < input.len() {
        let b0 = input[i] as usize;
        output.push(ALPHABET[b0 >> 2] as char);
        if i + 1 < input.len() {
            let b1 = input[i + 1] as usize;
            output.push(ALPHABET[((b0 & 3) << 4) | (b1 >> 4)] as char);
            output.push(ALPHABET[(b1 & 0xf) << 2] as char);
        } else {
            output.push(ALPHABET[(b0 & 3) << 4] as char);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tripcode_stable() {
        let (_, t1) = parse_name_tripcode("Anon#password123");
        let (_, t2) = parse_name_tripcode("DifferentName#password123");
        // Same password → same tripcode regardless of name
        assert_eq!(t1, t2);
        assert!(t1.unwrap().starts_with('!'));
    }

    #[test]
    fn test_no_tripcode() {
        let (name, trip) = parse_name_tripcode("Anonymous");
        assert_eq!(name, "Anonymous");
        assert!(trip.is_none());
    }

    #[test]
    fn test_empty_name() {
        let (name, trip) = parse_name_tripcode("#somepassword");
        assert_eq!(name, "Anonymous");
        assert!(trip.is_some());
    }
}
