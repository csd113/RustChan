//! Browser provenance validation shared by mutation preflight and CSRF guards.

use super::SecureCookieContext;
use axum::http::{header, HeaderMap, Uri};

/// Why request provenance could not establish the destination origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "the middleware facade exposes this result only within the crate"
)]
pub(crate) enum OriginRejection {
    /// No browser provenance establishes the request origin.
    Missing,
    /// Supplied provenance was malformed or ambiguous and cannot be bypassed.
    Invalid,
    /// The browser explicitly identified a different origin or site.
    CrossOrigin,
}

impl From<OriginRejection> for crate::error::AppError {
    fn from(rejection: OriginRejection) -> Self {
        Self::Forbidden(
            match rejection {
                OriginRejection::Missing => "Missing Origin/Referer header.",
                OriginRejection::Invalid => "Invalid or ambiguous Origin/Referer headers.",
                OriginRejection::CrossOrigin => "Origin/Referer origin mismatch.",
            }
            .into(),
        )
    }
}

/// Read a singleton browser header without turning invalid evidence into absence.
fn singleton(
    headers: &HeaderMap,
    name: header::HeaderName,
) -> Result<Option<&str>, OriginRejection> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(OriginRejection::Invalid);
    }
    let value = value
        .to_str()
        .map_err(|_error| OriginRejection::Invalid)?
        .trim();
    if value.is_empty() {
        return Err(OriginRejection::Invalid);
    }
    Ok(Some(value))
}

/// Reject invalid ports and authorities instead of treating them as default ports.
fn authority_is_valid(authority: &axum::http::uri::Authority) -> bool {
    let value = authority.as_str();
    !authority.host().is_empty()
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'@' | b'\\' | b',' | b'%'))
        && value.strip_prefix(authority.host()).is_some_and(|suffix| {
            suffix.is_empty()
                || suffix.strip_prefix(':').is_some_and(|port| {
                    !port.is_empty()
                        && port.bytes().all(|byte| byte.is_ascii_digit())
                        && port.parse::<u16>().is_ok()
                })
        })
}

/// Parse a serialized Origin, or an absolute HTTP(S) Referer URL.
fn parse_source(value: &str, origin: bool) -> Result<Uri, OriginRejection> {
    let uri = value
        .parse::<Uri>()
        .map_err(|_error| OriginRejection::Invalid)?;
    let scheme = uri.scheme_str().ok_or(OriginRejection::Invalid)?;
    let authority = uri.authority().ok_or(OriginRejection::Invalid)?;
    if value.contains('#')
        || !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || !authority_is_valid(authority)
        || (origin && value.split_once("://").map(|(_, rest)| rest) != Some(authority.as_str()))
    {
        return Err(OriginRejection::Invalid);
    }
    Ok(uri)
}

/// Reject malformed, duplicate, and explicit cross-site browser evidence.
fn validate_origin_headers(headers: &HeaderMap) -> Result<(), OriginRejection> {
    if let Some(origin) = singleton(headers, header::ORIGIN)? {
        if origin != "null" {
            let _source = parse_source(origin, true)?;
        }
    }
    if let Some(referer) = singleton(headers, header::REFERER)? {
        let _source = parse_source(referer, false)?;
    }
    if singleton(headers, header::HeaderName::from_static("sec-fetch-site"))?
        .is_some_and(|site| site.eq_ignore_ascii_case("cross-site"))
    {
        return Err(OriginRejection::CrossOrigin);
    }
    Ok(())
}

/// Compare scheme, host, and effective port using the actual trusted transport.
#[expect(
    clippy::redundant_pub_crate,
    reason = "the shared middleware facade must remain crate-visible"
)]
pub(crate) fn check_same_origin(
    headers: &HeaderMap,
    context: SecureCookieContext,
) -> Result<(), OriginRejection> {
    validate_origin_headers(headers)?;
    let host = singleton(headers, header::HOST)?.ok_or(OriginRejection::Invalid)?;
    let destination = host
        .parse::<axum::http::uri::Authority>()
        .map_err(|_error| OriginRejection::Invalid)?;
    if !authority_is_valid(&destination) {
        return Err(OriginRejection::Invalid);
    }
    let https = context.direct_https
        || super::forwarded_proto_is_https(
            headers,
            context.peer,
            crate::config::CONFIG.behind_proxy,
        );
    let scheme = if https { "https" } else { "http" };
    let port = destination
        .port_u16()
        .unwrap_or(if https { 443 } else { 80 });
    let mut supplied = false;
    for (value, origin) in [
        (singleton(headers, header::ORIGIN)?, true),
        (singleton(headers, header::REFERER)?, false),
    ] {
        let Some(value) = value.filter(|value| !origin || *value != "null") else {
            continue;
        };
        let source = parse_source(value, origin)?;
        let authority = source.authority().ok_or(OriginRejection::Invalid)?;
        let source_scheme = source.scheme_str().ok_or(OriginRejection::Invalid)?;
        let source_port = authority.port_u16().unwrap_or_else(|| {
            if source_scheme.eq_ignore_ascii_case("https") {
                443
            } else {
                80
            }
        });
        if !source_scheme.eq_ignore_ascii_case(scheme)
            || !authority.host().eq_ignore_ascii_case(destination.host())
            || source_port != port
        {
            return Err(OriginRejection::CrossOrigin);
        }
        supplied = true;
    }
    if supplied
        || singleton(headers, header::HeaderName::from_static("sec-fetch-site"))?
            .is_some_and(|site| site.eq_ignore_ascii_case("same-origin"))
    {
        Ok(())
    } else {
        Err(OriginRejection::Missing)
    }
}
