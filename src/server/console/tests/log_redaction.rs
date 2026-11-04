//! Sanitizer regressions reproduced by the terminal audit.
use super::safe_log_line;

#[test]
fn escaped_credentials_userinfo_and_controls_are_not_displayed() {
    for line in [
        r#"password="FAKE\"FAKEtail" msg="failure""#,
        "remote=https://FAKE_TOKEN@example.invalid/x",
        "password_hash=FAKE_HASH",
        "\u{202e}running\u{1b}[31m",
    ] {
        let safe = safe_log_line(line);
        assert!(
            !safe.contains("FAKE"),
            "credential suffix or URL userinfo survived redaction"
        );
        assert!(
            !safe.contains('\u{202e}') && !safe.contains('\u{1b}'),
            "terminal control survived redaction"
        );
    }
}
