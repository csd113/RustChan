use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{request::Parts, StatusCode},
};
use std::net::SocketAddr;

#[derive(Clone, Copy, Debug, Default)]
pub struct RequestTransport {
    pub direct_https: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SecureCookieContext {
    pub peer: Option<SocketAddr>,
    pub direct_https: bool,
}

impl SecureCookieContext {
    pub const fn new(peer: Option<SocketAddr>, direct_https: bool) -> Self {
        Self { peer, direct_https }
    }
}

impl<S> FromRequestParts<S> for SecureCookieContext
where
    S: Send + Sync,
{
    type Rejection = StatusCode;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl std::future::Future<Output = std::result::Result<Self, Self::Rejection>> + Send {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|connect_info| connect_info.0);
        let direct_https = parts
            .extensions
            .get::<RequestTransport>()
            .is_some_and(|transport| transport.direct_https);
        std::future::ready(Ok(Self::new(peer, direct_https)))
    }
}
