use crate::config::CONFIG;
use axum::{
    extract::{ConnectInfo, FromRequestParts, Request},
    http::request::Parts,
};
use ipnet::IpNet;

use std::convert::Infallible;
use std::net::SocketAddr;

/// Returns the first non-empty address in a forwarded-for header.
fn forwarded_client_ip(value: &str) -> Option<String> {
    value
        .split(',')
        .map(str::trim)
        .find(|ip| !ip.is_empty())
        .and_then(|ip| ip.parse::<std::net::IpAddr>().ok())
        .map(|ip| ip.to_string())
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
                .is_ok_and(|network| network.contains(&addr.ip()))
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
        .and_then(|value| value.parse::<std::net::IpAddr>().ok())
    {
        return Some(value.to_string());
    }

    headers
        .get("x-forwarded-for")
        .and_then(|header_value| header_value.to_str().ok())
        .and_then(forwarded_client_ip)
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

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|connect_info| connect_info.0);

        std::future::ready(Ok(Self(resolved_client_ip(
            &parts.headers,
            peer,
            CONFIG.behind_proxy,
            &CONFIG.trusted_proxy_cidrs,
            CONFIG.enable_tor_support,
        ))))
    }
}

#[cfg(test)]
mod tests {
    use super::{forwarded_client_ip, resolved_client_ip, trusted_proxy_peer_with, ClientIp};
    use anyhow::{ensure, Context as _};
    use axum::extract::{ConnectInfo, FromRequestParts as _};
    use axum::http::{HeaderMap, HeaderValue};
    use futures::FutureExt as _;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::sync::Arc;

    #[test]
    fn client_ip_extractor_is_ready_with_or_without_peer_metadata() -> anyhow::Result<()> {
        let peer = SocketAddr::from(([198, 51, 100, 10], 8080));
        for (test_peer, expected) in [(Some(peer), "198.51.100.10"), (None, "unknown")] {
            let (mut parts, ()) = axum::http::Request::new(()).into_parts();
            if let Some(client_peer) = test_peer {
                let _previous_peer = parts.extensions.insert(ConnectInfo(client_peer));
            }
            let resolved = ClientIp::from_request_parts(&mut parts, &())
                .now_or_never()
                .context("client IP extraction should complete without yielding")??;
            ensure!(
                resolved.0 == expected,
                "client IP extraction changed for {peer:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn forwarded_ip_prefers_leftmost_hop() {
        assert_eq!(
            forwarded_client_ip("198.51.100.10, 203.0.113.7, 10.0.0.1").as_deref(),
            Some("198.51.100.10")
        );
    }

    #[test]
    fn forwarded_ip_skips_empty_entries() {
        assert_eq!(
            forwarded_client_ip(" , 198.51.100.10").as_deref(),
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
        drop(crate::detect::TOR_STREAM_TOKENS.insert(peer, Arc::from("tor:test-stream")));

        let mut headers = HeaderMap::new();
        drop(headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.10")));
        drop(headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.7, 127.0.0.1"),
        ));
        let trusted = vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()];

        let resolved = resolved_client_ip(&headers, Some(peer), true, &trusted, true);

        drop(crate::detect::TOR_STREAM_TOKENS.remove(&peer));
        assert_eq!(resolved, "tor:test-stream");
    }

    #[test]
    fn forwarded_headers_still_apply_for_non_tor_trusted_proxy_peer() {
        let peer = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_153);
        let mut headers = HeaderMap::new();
        drop(headers.insert("x-real-ip", HeaderValue::from_static("198.51.100.10")));
        let trusted = vec!["127.0.0.1/32".to_owned(), "::1/128".to_owned()];

        assert_eq!(
            resolved_client_ip(&headers, Some(peer), true, &trusted, true),
            "198.51.100.10"
        );
    }
    #[test]
    fn untrusted_forwarded_headers_do_not_control_identity() {
        let peer = SocketAddr::from(([198, 51, 100, 10], 8080));
        let mut headers = HeaderMap::new();
        drop(headers.insert("x-real-ip", HeaderValue::from_static("203.0.113.20")));
        drop(headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.21")));
        let trusted = vec!["127.0.0.1/32".to_owned()];
        assert_eq!(
            resolved_client_ip(&headers, Some(peer), true, &trusted, false),
            "198.51.100.10",
            "untrusted peers must not spoof counters or bans"
        );
    }

    #[test]
    fn malformed_forwarded_identity_falls_back_and_ipv6_is_canonical() {
        let peer = SocketAddr::from(([127, 0, 0, 1], 8080));
        let mut headers = HeaderMap::new();
        drop(headers.insert("x-real-ip", HeaderValue::from_static("invented-visitor")));
        drop(headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("also-invalid, 203.0.113.1"),
        ));
        let trusted = vec!["127.0.0.1/32".to_owned()];
        assert_eq!(
            resolved_client_ip(&headers, Some(peer), true, &trusted, false),
            "127.0.0.1",
            "malformed proxy identities must not create arbitrary counters"
        );
        drop(headers.insert(
            "x-real-ip",
            HeaderValue::from_static("2001:0db8:0:0:0:0:0:1"),
        ));
        assert_eq!(
            resolved_client_ip(&headers, Some(peer), true, &trusted, false),
            "2001:db8::1",
            "alternate IPv6 spellings must share visitor identity"
        );
    }
}
