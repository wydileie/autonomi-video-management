use std::env;
use std::net::SocketAddr;
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
use crate::state::AppState;

mod client;
mod error;
mod routes;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    let rest_addr = env::var("ANTD_REST_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".to_string());
    let bind_addr: SocketAddr = rest_addr.parse()?;
    let network = env::var("ANTD_NETWORK").unwrap_or_else(|_| "default".to_string());
    let client = Arc::new(connect_client().await?);

    let state = AppState {
        client,
        network,
        metrics: Arc::new(HttpMetrics::default()),
    };
    let service_metrics = state.metrics.clone();
    let app = routes::router(state)
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

    info!("Autonomi 2.0 compatibility gateway listening on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
