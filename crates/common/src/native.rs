//! Browser requests to native loopback services must name the listener and an
//! explicitly configured web origin. This applies even without the launcher proxy.
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::net::SocketAddr;
#[derive(Clone)]
pub struct NativeRequestPolicy {
    address: SocketAddr,
    origins: Vec<HeaderValue>,
}
impl NativeRequestPolicy {
    pub fn new(address: SocketAddr, origins: Vec<HeaderValue>) -> Self {
        Self { address, origins }
    }
    pub fn allows(&self, headers: &HeaderMap) -> bool {
        if !self.address.ip().is_loopback() {
            return true;
        }
        let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
            return false;
        };
        if !host.eq_ignore_ascii_case(&self.address.to_string())
            && !host.eq_ignore_ascii_case(&format!("localhost:{}", self.address.port()))
        {
            return false;
        }
        headers
            .get(header::ORIGIN)
            .is_none_or(|origin| self.origins.contains(origin))
    }
}
pub async fn enforce_native_request(
    State(policy): State<NativeRequestPolicy>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !policy.allows(request.headers()) {
        return (StatusCode::FORBIDDEN, "Unrecognized native Host or Origin").into_response();
    }
    next.run(request).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_rebinding_and_unconfigured_origins() {
        let policy = NativeRequestPolicy::new(
            "127.0.0.1:8000".parse().expect("address"),
            vec![HeaderValue::from_static("http://127.0.0.1:8080")],
        );
        let mut headers = HeaderMap::new();
        assert!(!policy.allows(&headers));
        headers.insert(header::HOST, HeaderValue::from_static("evil.example:8000"));
        assert!(!policy.allows(&headers));
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8000"));
        assert!(policy.allows(&headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.example"),
        );
        assert!(!policy.allows(&headers));
        headers.insert(
            header::ORIGIN,
            HeaderValue::from_static("http://127.0.0.1:8080"),
        );
        assert!(policy.allows(&headers));
    }
}
