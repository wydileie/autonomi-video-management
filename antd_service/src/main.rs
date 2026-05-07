use std::time::Duration as StdDuration;
use std::{fs, sync::Arc};

use autvid_common::HttpMetrics;
use axum::http::{header, HeaderName, Method, Request, Response};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
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
    if config.internal_token.is_none() && !config.bind_addr.ip().is_loopback() {
        anyhow::bail!(
            "ANTD_INTERNAL_TOKEN or ANTD_INTERNAL_TOKEN_FILE is required when ANTD_REST_ADDR ({}) is non-loopback",
            config.bind_addr
        );
    }
    fs::create_dir_all(&config.upload_temp_dir)?;
    let client = Arc::new(connect_client().await?);

    let state = AppState {
        client,
        network: config.network.clone(),
        metrics: Arc::new(HttpMetrics::default()),
        cost_cache: Arc::new(CostCache::new(
            config.cost_cache_ttl,
            config.cost_cache_max_entries,
        )),
        upload_temp_dir: config.upload_temp_dir.clone(),
        file_upload_max_bytes: config.file_upload_max_bytes,
    };
    let service_metrics = state.metrics.clone();
    let cors = cors_layer(&config);
    let app = routes::router(state, &config)
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
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

fn cors_layer(config: &Config) -> CorsLayer {
    let layer = CorsLayer::new().expose_headers([HeaderName::from_static("x-request-id")]);
    if config.cors_allowed_origins.is_empty() {
        return layer;
    }
    layer
        .allow_origin(AllowOrigin::list(config.cors_allowed_origins.clone()))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("x-request-id"),
            HeaderName::from_static("x-content-sha256"),
        ])
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    info!("shutdown signal received");
}
