//! Rust Streaming Service
//!
//! Responsibilities:
//! - Generate HLS manifests from video manifests stored on Autonomi.
//! - Proxy individual .ts segments by fetching them from the Autonomi network
//!   via the antd daemon REST API.

use std::{env, path::PathBuf, sync::Arc, time::Duration as StdDuration};

use autvid_common::{secret_env, shutdown_signal as wait_for_shutdown_signal};
use axum::http::{header, HeaderName, Method, Request, Response, StatusCode};
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::{info, info_span, Span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::antd_client::AntdRestClient;
use crate::cache::AppCache;
use crate::config::{cors_allowed_origins, request_timeout_from_env, CacheConfig};
use crate::state::AppState;

mod antd_client;
mod cache;
mod config;
mod hls;
mod metrics;
mod models;
mod routes;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if autvid_common::run_healthcheck_from_args(env::args())? {
        return Ok(());
    }

    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let antd_url = env::var("ANTD_URL").unwrap_or_else(|_| "http://localhost:8082".into());
    let antd_internal_token = secret_env("ANTD_INTERNAL_TOKEN", "ANTD_INTERNAL_TOKEN_FILE")?;
    let catalog_state_path = env::var("CATALOG_STATE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/video_catalog/catalog.json"));
    let catalog_bootstrap_address = env::var("PUBLISHED_CATALOG_ADDRESS")
        .or_else(|_| env::var("CATALOG_ADDRESS"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let cache_config = CacheConfig::from_env()?;
    let request_timeout = request_timeout_from_env()?;

    let antd = AntdRestClient::new(&antd_url, antd_internal_token)?;

    let state = AppState {
        antd,
        catalog_state_path,
        catalog_bootstrap_address,
        cache: Arc::new(AppCache::new(&cache_config)),
        cache_config,
        metrics: Arc::new(metrics::StreamMetrics::default()),
    };
    let cors_allowed_origins = cors_allowed_origins()?;

    let cors = CorsLayer::new()
        .allow_origin(cors_allowed_origins.clone())
        .allow_methods([Method::GET, Method::HEAD, Method::OPTIONS])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::RANGE,
            HeaderName::from_static("x-request-id"),
        ])
        .expose_headers([HeaderName::from_static("x-request-id")]);

    info!(
        cors_allowed_origins = ?cors_allowed_origins,
        catalog_cache_ttl_seconds = state.cache_config.catalog_ttl.as_secs(),
        manifest_cache_ttl_seconds = state.cache_config.manifest_ttl.as_secs(),
        segment_cache_ttl_seconds = state.cache_config.segment_ttl.as_secs(),
        segment_cache_max_bytes = state.cache_config.segment_max_bytes,
        request_timeout_seconds = request_timeout.as_secs(),
        "configured stream caches"
    );

    let service_metrics = state.metrics.clone();
    let app = routes::router()
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        .layer(cors)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let request_id = request
                        .headers()
                        .get("x-request-id")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("");
                    info_span!(
                        "http_request",
                        service = "rust_stream",
                        request_id = %request_id,
                        method = %request.method(),
                        uri = %request.uri(),
                        version = ?request.version(),
                    )
                })
                .on_response(
                    move |response: &Response<_>, latency: StdDuration, _span: &Span| {
                        service_metrics
                            .http
                            .record_request(response.status().as_u16(), latency);
                        info!(
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis(),
                            "request completed"
                        );
                    },
                ),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(state);

    let bind_port = env::var("RUST_STREAM_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8081);
    let bind_addr = format!("0.0.0.0:{bind_port}");
    info!("Listening on {}", bind_addr);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    wait_for_shutdown_signal().await;
    info!("shutdown signal received");
}

#[cfg(test)]
mod tests;
