use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tower_http::timeout::TimeoutLayer;

use crate::config::Config;
use crate::state::AppState;

pub(crate) mod data;
mod file;
mod health;
mod shared;
mod wallet;

pub(crate) fn router(state: AppState, config: &Config) -> Router {
    let internal_token = config.internal_token.clone();
    let v1_json_routes = Router::new()
        .route("/v1/wallet/address", get(wallet::wallet_address))
        .route("/v1/wallet/balance", get(wallet::wallet_balance))
        .route("/v1/wallet/approve", post(wallet::wallet_approve))
        .route("/v1/data/cost", post(data::data_cost))
        .route("/v1/data/public", post(data::data_put_public))
        .route(
            "/v1/data/public/{address}/raw",
            get(data::data_get_public_raw),
        )
        .route("/v1/data/public/{address}", get(data::data_get_public))
        .route_layer(middleware::from_fn_with_state(
            internal_token.clone(),
            require_internal_token,
        ))
        .route_layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            config.request_timeout,
        ));

    let v1_file_routes = Router::new()
        .route(
            "/v1/file/public",
            post(file::file_put_public).layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                config.file_upload_request_timeout,
            )),
        )
        .route_layer(middleware::from_fn_with_state(
            internal_token,
            require_internal_token,
        ));

    Router::new()
        .route("/health", get(health::health))
        .route("/metrics", get(metrics))
        .merge(v1_json_routes)
        .merge(v1_file_routes)
        .layer(DefaultBodyLimit::max(config.json_body_limit_bytes))
        .with_state(state)
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render_prometheus("antd_service"),
    )
}

async fn require_internal_token(
    State(expected): State<Option<String>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected) = expected else {
        return next.run(request).await;
    };
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(str::trim)
        .is_some_and(|token| token == expected);
    if authorized {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "internal bearer token required").into_response()
    }
}
