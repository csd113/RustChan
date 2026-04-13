// src/middleware/ip.rs

use crate::config::CONFIG;
use axum::extract::Request;
use ipnet::IpNet;
use std::net::SocketAddr;

fn forwarded_client_ip(value: &str) -> Option<&str> {
    value.split(',').map(str::trim).find(|ip| !ip.is_empty())
}

pub(crate) fn trusted_proxy_peer(peer: Option<SocketAddr>) -> bool {
    trusted_proxy_peer_with(peer, &CONFIG.trusted_proxy_cidrs)
}

fn trusted_proxy_peer_with(peer: Option<SocketAddr>, trusted_proxy_cidrs: &[String]) -> bool {
    peer.is_some_and(|addr| {
        trusted_proxy_cidrs.iter().any(|cidr| {
            cidr.parse::<IpNet>()
                .ok()
                .is_some_and(|network| network.contains(&addr.ip()))
        })
    })
}

pub(crate) fn forwarded_proto_is_https(
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

fn forwarded_ip_from_headers(
    headers: &axum::http::HeaderMap,
    peer: Option<SocketAddr>,
) -> Option<String> {
    if !CONFIG.behind_proxy || !trusted_proxy_peer(peer) {
        return None;
    }

    if let Some(value) = headers
        .get("x-real-ip")
        .and_then(|header_value| header_value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(value.to_string());
    }

    headers
        .get("x-forwarded-for")
        .and_then(|header_value| header_value.to_str().ok())
        .and_then(forwarded_client_ip)
        .map(ToString::to_string)
}

pub fn extract_ip(req: &Request) -> String {
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|connect_info| connect_info.0);

    if let Some(ip) = forwarded_ip_from_headers(req.headers(), peer) {
        return ip;
    }

    if CONFIG.enable_tor_support {
        if let Some(addr) = peer {
            if addr.ip().is_loopback() {
                if let Some(token) = crate::detect::TOR_STREAM_TOKENS.get(&addr.port()) {
                    return token.value().to_string();
                }
            }
        }
    }

    peer.map_or_else(|| "unknown".to_string(), |addr| addr.ip().to_string())
}

pub struct ClientIp(pub String);

impl<S> axum::extract::FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|connect_info| connect_info.0);

        if let Some(ip) = forwarded_ip_from_headers(&parts.headers, peer) {
            return Ok(Self(ip));
        }

        if CONFIG.enable_tor_support {
            if let Some(addr) = peer {
                if addr.ip().is_loopback() {
                    if let Some(token) = crate::detect::TOR_STREAM_TOKENS.get(&addr.port()) {
                        return Ok(Self(token.value().to_string()));
                    }
                }
            }
        }

        Ok(Self(peer.map_or_else(
            || "unknown".to_string(),
            |addr| addr.ip().to_string(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{forwarded_client_ip, trusted_proxy_peer_with};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

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
            "127.0.0.1/32".to_string(),
            "::1/128".to_string(),
            "10.0.0.0/8".to_string(),
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
        let trusted = vec!["127.0.0.1/32".to_string(), "::1/128".to_string()];
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
        let trusted = vec!["127.0.0.1/32".to_string(), "::1/128".to_string()];
        assert!(!trusted_proxy_peer_with(
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                8080,
            )),
            &trusted
        ));
    }
}
