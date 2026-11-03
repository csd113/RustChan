//! Internal upload rejection sources, independent of diagnostic wording.

/// Client-actionable upload failures carried through existing anyhow results.
#[derive(Debug, thiserror::Error)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "typed sources cross private owners without widening the public upload API"
)]
pub(crate) enum UploadRejection {
    /// Byte or free-space allowance exceeded; the message is safe for clients.
    #[error("{0}")]
    TooLarge(String),
    /// Content type is prohibited; the message is safe for clients.
    #[error("{0}")]
    Disallowed(String),
    /// Malformed image header with a dedicated, sanitized recovery message.
    #[error("{0}")]
    MalformedImageHeader(String),
    /// Other invalid input; decoder details remain private at the HTTP boundary.
    #[error("{0}")]
    Malformed(String),
}
