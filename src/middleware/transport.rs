use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{request::Parts, StatusCode},
};
use std::future::Future;
use std::net::SocketAddr;

#[derive(Clone, Copy, Debug, Default)]
/// Transport properties recorded by the server before routing a request.
pub struct RequestTransport {
    /// Whether the server received the request over a direct HTTPS connection.
    pub direct_https: bool,
}

#[derive(Clone, Copy, Debug, Default)]
/// Connection metadata used to decide whether cookies require the secure flag.
pub struct SecureCookieContext {
    /// Immediate network peer, when connection metadata is available.
    pub peer: Option<SocketAddr>,
    /// Whether the request arrived over a direct HTTPS connection.
    pub direct_https: bool,
}

impl SecureCookieContext {
    /// Creates cookie context from the immediate peer and direct transport.
    #[must_use]
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
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
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
