// src/middleware/ip.rs

use crate::config::CONFIG;
use axum::extract::Request;
use std::net::{IpAddr, SocketAddr};

fn forwarded_client_ip(value: &str) -> Option<&str> {
    value.split(',').map(str::trim).find(|ip| !ip.is_empty())
}

fn trusted_proxy_peer(peer: Option<SocketAddr>) -> bool {
    peer.is_some_and(|addr| match addr.ip() {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
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
    use super::{forwarded_client_ip, trusted_proxy_peer};
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
        assert!(trusted_proxy_peer(Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            8080,
        ))));
        assert!(trusted_proxy_peer(Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            8080,
        ))));
        assert!(trusted_proxy_peer(Some(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            8080,
        ))));
    }

    #[test]
    fn trusted_proxy_rejects_public_internet_peers() {
        assert!(!trusted_proxy_peer(Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)),
            8080,
        ))));
        assert!(!trusted_proxy_peer(None));
    }
}
