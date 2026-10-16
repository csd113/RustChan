use crate::config::CONFIG;
use axum::{
    extract::{ConnectInfo, FromRequestParts, Request},
    http::request::Parts,
};
use ipnet::IpNet;

use std::convert::Infallible;
use std::net::SocketAddr;

/// Walk an appended proxy chain from the trusted peer toward the first untrusted hop.
fn forwarded_client_ip(
    value: &str,
    peer: SocketAddr,
    trusted_proxy_cidrs: &[String],
) -> Option<String> {
    let mut client = peer.ip();
    for forwarded in value.rsplit(',') {
        if !trusted_proxy_peer_with(Some(SocketAddr::new(client, 0)), trusted_proxy_cidrs) {
            break;
        }
        client = forwarded.trim().parse::<std::net::IpAddr>().ok()?;
    }
    Some(client.to_string())
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
                .rsplit(',')
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

    // An appending proxy preserves attacker-supplied left-hand values. Prefer
    // its authoritative chain over X-Real-IP and stop at the first untrusted hop.
    if headers.contains_key("x-forwarded-for") {
        let mut values = headers.get_all("x-forwarded-for").iter();
        let value = values.next()?.to_str().ok()?;
        if values.next().is_some() {
            return None;
        }
        return forwarded_client_ip(value, peer?, trusted_proxy_cidrs);
    }

    if let Some(value) = headers
        .get("x-real-ip")
        .and_then(|header_value| header_value.to_str().ok())
        .map(str::trim)
        .and_then(|value| value.parse::<std::net::IpAddr>().ok())
    {
        return Some(value.to_string());
    }

    None
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
