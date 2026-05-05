use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use tower_http::timeout::TimeoutLayer;

use crate::config::Config;
use crate::state::AppState;

mod data;
mod file;
mod health;
mod shared;
mod wallet;

pub(crate) fn router(state: AppState, config: &Config) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/metrics", get(metrics))
        .route("/v1/wallet/address", get(wallet::wallet_address))
        .route("/v1/wallet/balance", get(wallet::wallet_balance))
        .route("/v1/wallet/approve", post(wallet::wallet_approve))
        .route("/v1/data/cost", post(data::data_cost))
        .route("/v1/data/public", post(data::data_put_public))
        .route("/v1/data/public/{address}", get(data::data_get_public))
        .route_layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            config.request_timeout,
        ))
        .route(
            "/v1/file/public",
            post(file::file_put_public).layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                config.file_upload_request_timeout,
            )),
        )
        .with_state(state)
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render_prometheus("antd_service"),
    )
}
