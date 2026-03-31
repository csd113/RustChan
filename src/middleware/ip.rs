use crate::config::CONFIG;
use axum::extract::Request;

pub fn extract_ip(req: &Request) -> String {
    if CONFIG.behind_proxy {
        if let Some(real_ip) = req.headers().get("x-real-ip") {
            if let Ok(value) = real_ip.to_str() {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }

        if let Some(fwd) = req.headers().get("x-forwarded-for") {
            if let Ok(value) = fwd.to_str() {
                if let Some(ip) = value.split(',').next_back() {
                    let trimmed = ip.trim();
                    if !trimmed.is_empty() {
                        return trimmed.to_string();
                    }
                }
            }
        }
    }

    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|connect_info| connect_info.0);

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
        if CONFIG.behind_proxy {
            if let Some(value) = parts
                .headers
                .get("x-real-ip")
                .and_then(|header_value| header_value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Ok(Self(value.to_string()));
            }

            if let Some(value) = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|header_value| header_value.to_str().ok())
                .and_then(|value| value.split(',').next_back())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Ok(Self(value.to_string()));
            }
        }

        let peer = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|connect_info| connect_info.0);

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
