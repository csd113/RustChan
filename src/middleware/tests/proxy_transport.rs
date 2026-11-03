//! Preserve trusted-proxy transport decisions across equivalent header encodings.

use super::forwarded_proto_is_https_with;
use anyhow::{ensure, Result};
use axum::http::{HeaderMap, HeaderValue};

#[test]
fn appended_protocol_is_authoritative_only_for_trusted_peers() -> Result<()> {
    let trusted = Some("127.0.0.1:1234".parse()?);
    let untrusted = Some("192.0.2.1:1234".parse()?);
    let cidrs = ["127.0.0.1/32".to_owned()];
    let mut headers = HeaderMap::new();
    let _appended = headers.append("x-forwarded-proto", HeaderValue::from_static("http"));
    let _secure_hop = headers.append("x-forwarded-proto", HeaderValue::from_static("https"));
    ensure!(forwarded_proto_is_https_with(
        &headers, trusted, true, &cidrs
    ));
    ensure!(!forwarded_proto_is_https_with(
        &headers, untrusted, true, &cidrs
    ));
    ensure!(!forwarded_proto_is_https_with(
        &headers, trusted, false, &cidrs
    ));
    drop(headers.insert("x-forwarded-proto", HeaderValue::from_static("http, https")));
    ensure!(forwarded_proto_is_https_with(
        &headers, trusted, true, &cidrs
    ));
    let _final_hop = headers.append("x-forwarded-proto", HeaderValue::from_static("http"));
    ensure!(!forwarded_proto_is_https_with(
        &headers, trusted, true, &cidrs
    ));
    Ok(())
}
