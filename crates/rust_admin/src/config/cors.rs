use std::env;

use axum::http::{header, HeaderName, HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

use super::*;

pub(crate) fn cors_layer(config: &Config) -> anyhow::Result<CorsLayer> {
    Ok(CorsLayer::new()
        .allow_origin(AllowOrigin::list(config.cors_allowed_origins.clone()))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::COOKIE,
            header::CONTENT_TYPE,
            header::RANGE,
            HeaderName::from_static("x-request-id"),
            HeaderName::from_static("x-csrf-token"),
        ])
        .allow_credentials(true)
        .expose_headers([HeaderName::from_static("x-request-id")]))
}

pub(crate) fn cors_allowed_origins() -> anyhow::Result<Vec<HeaderValue>> {
    let raw = env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost,http://127.0.0.1".into());
    autvid_common::parse_cors_allowed_origins(&raw)
}
