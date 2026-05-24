use ::captcha::{
    filters::{Dots, Grid, Noise, Wave},
    Captcha,
};
use dashmap::DashMap;
use sha2::{Digest as _, Sha256};
use std::sync::LazyLock;
use subtle::ConstantTimeEq as _;

const CAPTCHA_ID_LEN: usize = 32;
const CAPTCHA_ANSWER_LEN: u32 = 5;
const CAPTCHA_MAX_ANSWER_LEN: usize = 16;
const CAPTCHA_TTL_SECS: i64 = 300;
const CAPTCHA_CHARSET: &[char] = &[
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'M', 'N', 'P', 'Q', 'R', 'S', 'T', 'U', 'V',
    'W', 'X', 'Y', 'Z', '2', '3', '4', '5', '6', '7', '8', '9',
];

static CAPTCHA_CHALLENGES: LazyLock<DashMap<String, StoredCaptchaChallenge>> =
    LazyLock::new(DashMap::new);

#[derive(Clone)]
struct StoredCaptchaChallenge {
    board_short: String,
    answer_hash: [u8; 32],
    expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptchaImageError {
    InvalidRequest,
    GenerationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptchaValidationError {
    Missing,
    Expired,
    Incorrect,
}

impl CaptchaValidationError {
    #[must_use]
    pub const fn user_message(self) -> &'static str {
        match self {
            Self::Missing => {
                "CAPTCHA verification failed. Enter the text from the image and try again."
            }
            Self::Expired => "CAPTCHA expired. Request a new challenge and try again.",
            Self::Incorrect => {
                "CAPTCHA verification failed. Request a new challenge and try again."
            }
        }
    }
}

#[must_use]
pub fn new_captcha_id() -> String {
    crate::utils::crypto::random_hex(CAPTCHA_ID_LEN / 2)
}

/// Generate and store a fresh CAPTCHA image for an existing form challenge id.
///
/// # Errors
/// Returns an error if the id or board name is malformed, or if the image crate
/// fails to encode a PNG.
pub fn generate_captcha_image(
    board_short: &str,
    captcha_id: &str,
) -> Result<Vec<u8>, CaptchaImageError> {
    if !is_valid_board_short(board_short) || !is_valid_captcha_id(captcha_id) {
        return Err(CaptchaImageError::InvalidRequest);
    }

    prune_expired(chrono::Utc::now().timestamp());

    let mut captcha = Captcha::new();
    captcha
        .set_chars(CAPTCHA_CHARSET)
        .add_chars(CAPTCHA_ANSWER_LEN)
        .apply_filter(Noise::new(0.25))
        .apply_filter(Grid::new(8, 8))
        .apply_filter(Wave::new(2.0, 12.0))
        .view(220, 120)
        .apply_filter(Dots::new(12).max_radius(6).min_radius(3));

    let (answer, png) = captcha
        .as_tuple()
        .ok_or(CaptchaImageError::GenerationFailed)?;
    store_challenge(
        board_short,
        captcha_id,
        &answer,
        chrono::Utc::now().timestamp(),
    );
    Ok(png)
}

/// Validate and consume a submitted CAPTCHA.
///
/// A challenge is removed for every validation attempt. This keeps successful
/// answers from being replayed and makes guessing impractical because each
/// challenge permits only one answer attempt within its short lifetime.
///
/// # Errors
/// Returns an error when the request is malformed, the challenge is missing or
/// expired, or the submitted answer does not match the stored challenge.
pub fn verify_captcha(
    board_short: &str,
    captcha_id: &str,
    submitted_answer: &str,
) -> Result<(), CaptchaValidationError> {
    if !is_valid_board_short(board_short)
        || !is_valid_captcha_id(captcha_id)
        || normalize_answer(submitted_answer).is_none()
    {
        return Err(CaptchaValidationError::Missing);
    }

    let now = chrono::Utc::now().timestamp();
    prune_expired(now);

    let Some((_, challenge)) = CAPTCHA_CHALLENGES.remove(captcha_id) else {
        return Err(CaptchaValidationError::Expired);
    };

    if challenge.expires_at <= now {
        return Err(CaptchaValidationError::Expired);
    }

    if challenge.board_short != board_short {
        return Err(CaptchaValidationError::Incorrect);
    }

    let submitted_hash = answer_hash(board_short, captcha_id, submitted_answer)
        .ok_or(CaptchaValidationError::Missing)?;
    if bool::from(challenge.answer_hash.ct_eq(&submitted_hash)) {
        Ok(())
    } else {
        Err(CaptchaValidationError::Incorrect)
    }
}

fn store_challenge(board_short: &str, captcha_id: &str, answer: &str, now: i64) {
    if let Some(answer_hash) = answer_hash(board_short, captcha_id, answer) {
        CAPTCHA_CHALLENGES.insert(
            captcha_id.to_owned(),
            StoredCaptchaChallenge {
                board_short: board_short.to_owned(),
                answer_hash,
                expires_at: now.saturating_add(CAPTCHA_TTL_SECS),
            },
        );
    }
}

fn prune_expired(now: i64) {
    CAPTCHA_CHALLENGES.retain(|_, challenge| challenge.expires_at > now);
}

fn answer_hash(board_short: &str, captcha_id: &str, answer: &str) -> Option<[u8; 32]> {
    let normalized = normalize_answer(answer)?;
    let mut hasher = Sha256::new();
    hasher.update(crate::config::CONFIG.cookie_secret.as_bytes());
    hasher.update(b":captcha:");
    hasher.update(board_short.as_bytes());
    hasher.update(b":");
    hasher.update(captcha_id.as_bytes());
    hasher.update(b":");
    hasher.update(normalized.as_bytes());
    Some(hasher.finalize().into())
}

fn normalize_answer(answer: &str) -> Option<String> {
    let answer = answer.trim();
    if answer.is_empty()
        || answer.len() > CAPTCHA_MAX_ANSWER_LEN
        || !answer.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(answer.to_ascii_uppercase())
}

fn is_valid_captcha_id(id: &str) -> bool {
    id.len() == CAPTCHA_ID_LEN && id.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_valid_board_short(board_short: &str) -> bool {
    !board_short.is_empty()
        && board_short.len() <= 32
        && board_short
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
pub mod testing {
    pub fn insert_challenge_for_test(
        board_short: &str,
        captcha_id: &str,
        answer: &str,
        expires_at: i64,
    ) {
        let Some(answer_hash) = super::answer_hash(board_short, captcha_id, answer) else {
            return;
        };
        super::CAPTCHA_CHALLENGES.insert(
            captcha_id.to_owned(),
            super::StoredCaptchaChallenge {
                board_short: board_short.to_owned(),
                answer_hash,
                expires_at,
            },
        );
    }

    pub fn challenge_exists_for_test(captcha_id: &str) -> bool {
        super::CAPTCHA_CHALLENGES.contains_key(captcha_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{testing, *};

    #[test]
    fn generated_captcha_is_png_and_stores_server_side_answer() {
        let captcha_id = "00000000000000000000000000000001";

        let png = generate_captcha_image("test", captcha_id).expect("captcha image");

        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(testing::challenge_exists_for_test(captcha_id));
    }

    #[test]
    fn captcha_success_consumes_challenge_and_rejects_replay() {
        let captcha_id = "00000000000000000000000000000002";
        testing::insert_challenge_for_test(
            "test",
            captcha_id,
            "AbC23",
            chrono::Utc::now().timestamp() + CAPTCHA_TTL_SECS,
        );

        assert_eq!(verify_captcha("test", captcha_id, "abc23"), Ok(()));
        assert_eq!(
            verify_captcha("test", captcha_id, "abc23"),
            Err(CaptchaValidationError::Expired)
        );
    }

    #[test]
    fn wrong_answer_consumes_challenge() {
        let captcha_id = "00000000000000000000000000000003";
        testing::insert_challenge_for_test(
            "test",
            captcha_id,
            "ABC23",
            chrono::Utc::now().timestamp() + CAPTCHA_TTL_SECS,
        );

        assert_eq!(
            verify_captcha("test", captcha_id, "WRONG"),
            Err(CaptchaValidationError::Incorrect)
        );
        assert_eq!(
            verify_captcha("test", captcha_id, "ABC23"),
            Err(CaptchaValidationError::Expired)
        );
    }

    #[test]
    fn expired_challenge_is_rejected() {
        let captcha_id = "00000000000000000000000000000004";
        testing::insert_challenge_for_test(
            "test",
            captcha_id,
            "ABC23",
            chrono::Utc::now().timestamp() - 1,
        );

        assert_eq!(
            verify_captcha("test", captcha_id, "ABC23"),
            Err(CaptchaValidationError::Expired)
        );
    }

    #[test]
    fn malformed_submission_is_missing() {
        let captcha_id = "00000000000000000000000000000005";
        assert_eq!(
            verify_captcha("test", captcha_id, ""),
            Err(CaptchaValidationError::Missing)
        );
        assert_eq!(
            verify_captcha("test", "not-an-id", "ABC23"),
            Err(CaptchaValidationError::Missing)
        );
    }
}
