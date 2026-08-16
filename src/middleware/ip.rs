// src/middleware/ip.rs

use crate::config::CONFIG;
use axum::{
    extract::{ConnectInfo, FromRequestParts, Request},
    http::request::Parts,
};
use ipnet::IpNet;

use std::convert::Infallible;
use std::net::SocketAddr;

/// Returns the first non-empty address in a forwarded-for header.
fn forwarded_client_ip(value: &str) -> Option<&str> {
    value.split(',').map(str::trim).find(|ip| !ip.is_empty())
}

/// Returns whether the peer belongs to a configured trusted proxy network.
fn trusted_proxy_peer(peer: Option<SocketAddr>) -> bool {
    trusted_proxy_peer_with(peer, &CONFIG.trusted_proxy_cidrs)
}

/// Tests a peer against an explicit trusted-proxy CIDR list.
fn trusted_proxy_peer_with(peer: Option<SocketAddr>, trusted_proxy_cidrs: &[String]) -> bool {
    peer.is_some_and(|addr| {
        trusted_proxy_cidrs.iter().any(|cidr| {
            cidr.parse::<IpNet>()
                .ok()
                .is_some_and(|network| network.contains(&addr.ip()))
        })
    })
}

/// Returns whether a trusted proxy reported HTTPS for the request.
pub(super) fn forwarded_proto_is_https(
    headers: &axum::http::HeaderMap,
    peer: Option<SocketAddr>,
    behind_proxy: bool,
) -> bool {
    if !behind_proxy || !trusted_proxy_peer(peer) {
        return false;
    }

    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .next()
                .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
        })
}

/// Extracts a forwarded client address when the immediate peer is trusted.
fn forwarded_ip_from_headers_with(
    headers: &axum::http::HeaderMap,
    peer: Option<SocketAddr>,
    behind_proxy: bool,
    trusted_proxy_cidrs: &[String],
) -> Option<String> {
    if !behind_proxy || !trusted_proxy_peer_with(peer, trusted_proxy_cidrs) {
        return None;
    }

    if let Some(value) = headers
        .get("x-real-ip")
        .and_then(|header_value| header_value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(value.to_owned());
    }

    headers
        .get("x-forwarded-for")
        .and_then(|header_value| header_value.to_str().ok())
        .and_then(forwarded_client_ip)
        .map(str::to_owned)
}

/// Resolves the effective client identity from Tor, proxy, or peer metadata.
fn resolved_client_ip(
    headers: &axum::http::HeaderMap,
    peer: Option<SocketAddr>,
    behind_proxy: bool,
    trusted_proxy_cidrs: &[String],
    enable_tor_support: bool,
) -> String {
    if let Some(token) = crate::detect::tor_stream_token_identity(peer, enable_tor_support) {
        return token;
    }

    if let Some(ip) =
        forwarded_ip_from_headers_with(headers, peer, behind_proxy, trusted_proxy_cidrs)
    {
        return ip;
    }

    peer.map_or_else(|| "unknown".to_owned(), |addr| addr.ip().to_string())
}

/// Extracts the effective client identity from an Axum request.
pub fn extract_ip(req: &Request) -> String {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0);

    resolved_client_ip(
        req.headers(),
        peer,
        CONFIG.behind_proxy,
        &CONFIG.trusted_proxy_cidrs,
        CONFIG.enable_tor_support,
    )
}

/// Axum extractor for the effective client identity.
#[derive(Debug)]
pub struct ClientIp(pub String);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|connect_info| connect_info.0);

        Ok(Self(resolved_client_ip(
            &parts.headers,
            peer,
            CONFIG.behind_proxy,
            &CONFIG.trusted_proxy_cidrs,
            CONFIG.enable_tor_support,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{forwarded_client_ip, resolved_client_ip, trusted_proxy_peer_with};
    use axum::http::{HeaderMap, HeaderValue};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::sync::Arc;

    #[test]
    fn forwarded_ip_prefers_leftmost_hop() {
        assert_eq!(
            forwarded_client_ip("198.51.100.10, 203.0.113.7, 10.0.0.1"),
            Some("198.51.100.10")
        );
    }

    #[test]
    fn forwarded_ip_skips_empty_entries() {
        assert_eq!(
            forwarded_client_ip(" , 198.51.100.10"),
            Some("198.51.100.10")
        );
    }

    #[test]
    fn trusted_proxy_accepts_loopback_and_private_networks() {
        let trusted = vec![
            "127.0.0.1/32".to_owned(),
            "::1/128".to_owned(),
            "10.0.0.0/8".to_owned(),
        ];
        assert!(trusted_proxy_peer_with(
            Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8080,)),
            &trusted
        ));
        assert!(trusted_proxy_peer_with(
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                8080,
            )),
            &trusted
        ));
        assert!(trusted_proxy_peer_with(
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8080,)),
            &trusted
        ));
    }

    #[test]
    fn trusted_proxy_rejects_public_internet_peers() {
        let trusted = vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()];
        assert!(!trusted_proxy_peer_with(
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)),
                8080,
            )),
            &trusted
        ));
        assert!(!trusted_proxy_peer_with(None, &trusted));
    }

    #[test]
    fn trusted_proxy_rejects_private_peers_not_in_allowlist() {
        let trusted = vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()];
        assert!(!trusted_proxy_peer_with(
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                8080,
            )),
            &trusted
        ));
    }

    #[test]
    fn tor_stream_token_precedes_spoofed_forwarded_headers_for_loopback_peer() {
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_152);
        crate::detect::TOR_STREAM_TOKENS.insert(peer, Arc::from("tor:test-stream"));

        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.10"));
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.7, 127.0.0.1"),
        );
        let trusted = vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()];

        let resolved = resolved_client_ip(&headers, Some(peer), true, &trusted, true);

        crate::detect::TOR_STREAM_TOKENS.remove(&peer);
        assert_eq!(resolved, "tor:test-stream");
    }

    #[test]
    fn forwarded_headers_still_apply_for_non_tor_trusted_proxy_peer() {
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_153);
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.10"));
        let trusted = vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()];

        assert_eq!(
            resolved_client_ip(&headers, Some(peer), true, &trusted, true),
            "198.51.100.10"
        );
    }
}
