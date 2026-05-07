use std::sync::Arc;
use std::time::Duration as StdDuration;

use autvid_common::HttpMetrics;
use axum::http::{HeaderName, Request, Response};
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::{info, info_span, Span};

use crate::client::{connect_client, init_logging};
use crate::config::Config;
use crate::state::{AppState, CostCache};

mod client;
mod config;
mod error;
mod routes;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    let config = Config::from_env()?;
    let client = Arc::new(connect_client().await?);

    let state = AppState {
        client,
        network: config.network.clone(),
        metrics: Arc::new(HttpMetrics::default()),
        cost_cache: Arc::new(CostCache::new(
            config.cost_cache_ttl,
            config.cost_cache_max_entries,
        )),
    };
    let service_metrics = state.metrics.clone();
    let app = routes::router(state, &config)
        .layer(CorsLayer::permissive().expose_headers([HeaderName::from_static("x-request-id")]))
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
                        service = "antd_service",
                        request_id = %request_id,
                        method = %request.method(),
                        uri = %request.uri(),
                        version = ?request.version(),
                    )
                })
                .on_response(
                    move |response: &Response<_>, latency: StdDuration, _span: &Span| {
                        service_metrics.record_request(response.status().as_u16(), latency);
                        info!(
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis(),
                            "request completed"
                        );
                    },
                ),
        )
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));

    info!(
        request_timeout_seconds = config.request_timeout.as_secs(),
        file_upload_request_timeout_seconds = config.file_upload_request_timeout.as_secs(),
        "Autonomi 2.0 compatibility gateway listening on {}",
        config.bind_addr,
    );
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
