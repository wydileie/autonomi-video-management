use autvid_common::constant_time_eq;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tower_http::timeout::TimeoutLayer;

use crate::config::Config;
use crate::state::AppState;

pub(crate) mod data;
mod download;
mod file;
mod health;
mod shared;
mod wallet;

pub(crate) fn router(state: AppState, config: &Config) -> Router {
    let internal_token = (config.internal_token.clone(), config.read_token.clone());
    let v1_json_routes = Router::new()
        .route("/v1/file/public/{address}/raw", get(download::file_raw))
        .route("/v1/wallet/address", get(wallet::wallet_address))
        .route("/v1/wallet/balance", get(wallet::wallet_balance))
        .route("/v1/wallet/approve", post(wallet::wallet_approve))
        .route("/v1/payments/lease", post(crate::payments::activate))
        .route("/v1/payments/approvals", post(crate::payments::approve))
        .route(
            "/v1/payments/approvals/{id}/cancel-unpaid",
            post(crate::payments::cancel_unpaid),
        )
        .route(
            "/v1/payments/uploads/{id}/resume",
            post(crate::payments::resume),
        )
        .route("/v1/payments/uploads/{id}", get(crate::payments::status))
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

    let receive_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(4));
    let v1_file_routes = Router::new()
        .route(
            "/v1/file/cost",
            post(file::file_cost).layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                config.file_upload_request_timeout,
            )),
        )
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
        ))
        .route_layer(middleware::from_fn_with_state(
            receive_slots,
            limit_file_requests,
        ));

    Router::new()
        .route("/livez", get(livez))
        .route("/health", get(health::health))
        .route("/metrics", get(metrics))
        .merge(v1_json_routes)
        .merge(v1_file_routes)
        .layer(DefaultBodyLimit::max(config.json_body_limit_bytes))
        .layer(middleware::from_fn_with_state(
            autvid_common::native::NativeRequestPolicy::new(
                config.bind_addr,
                config.cors_allowed_origins.clone(),
            ),
            autvid_common::native::enforce_native_request,
        ))
        .with_state(state)
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render_prometheus("antd_service"),
    )
}

async fn livez() -> impl IntoResponse {
    StatusCode::OK
}

async fn require_internal_token(
    State((expected, read_token)): State<(Option<String>, Option<String>)>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected) = expected else {
        return next.run(request).await;
    };
    let read_only = request.method() == axum::http::Method::GET
        && (request.uri().path().starts_with("/v1/data/public/")
            || request.uri().path().starts_with("/v1/file/public/"));
    let authorized = internal_token_authorized(request.headers(), &expected)
        || (read_only
            && read_token
                .as_deref()
                .is_some_and(|token| internal_token_authorized(request.headers(), token)));
    if authorized {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "internal bearer token required").into_response()
    }
}

fn internal_token_authorized(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(str::trim)
        .is_some_and(|token| constant_time_eq(token, expected))
}

async fn limit_file_requests(
    State(slots): State<std::sync::Arc<tokio::sync::Semaphore>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Ok(_permit) = slots.try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "file request capacity exhausted",
        )
            .into_response();
    };
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::http::{header, HeaderMap, HeaderValue};

    use super::internal_token_authorized;

    #[test]
    fn internal_token_auth_rejects_missing_or_wrong_bearer() {
        let expected = "secret-token";
        let mut headers = HeaderMap::new();
        assert!(!internal_token_authorized(&headers, expected));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong-token"),
        );
        assert!(!internal_token_authorized(&headers, expected));

        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );
        assert!(internal_token_authorized(&headers, expected));
    }
}
