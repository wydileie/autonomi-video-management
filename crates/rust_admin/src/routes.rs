use std::time::Duration as StdDuration;

use axum::{
    extract::DefaultBodyLimit,
    http::{Request, Response, StatusCode},
    routing::{get, patch, post},
    Router,
};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::{info, info_span, Span};

use crate::{
    auth::{auth_me, login, logout, refresh},
    config::{cors_layer, duration_from_secs_f64, Config},
    state::AppState,
};

mod admin;
mod health;
mod public;
mod upload;

pub fn router(config: &Config, state: AppState) -> anyhow::Result<Router> {
    let service_metrics = state.metrics.clone();
    let default_timeout = TimeoutLayer::with_status_code(
        StatusCode::REQUEST_TIMEOUT,
        duration_from_secs_f64(config.admin_request_timeout_seconds),
    );
    let upload_timeout = TimeoutLayer::with_status_code(
        StatusCode::REQUEST_TIMEOUT,
        duration_from_secs_f64(config.admin_upload_request_timeout_seconds),
    );
    Ok(Router::new()
        .route("/livez", get(health::livez))
        .route("/health", get(health::health))
        .route("/metrics", get(health::metrics))
        .route("/auth/login", post(login))
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(auth_me))
        .route("/catalog", get(public::get_catalog))
        .route("/videos/upload/quote", post(upload::quote_video_upload))
        .route("/videos", get(public::list_videos))
        .route("/admin/catalogs", get(admin::admin_get_catalogs))
        .route(
            "/admin/catalogs/publish",
            post(admin::admin_publish_catalogs),
        )
        .route("/admin/videos", get(admin::admin_list_videos))
        .route(
            "/videos/{video_id}",
            get(public::get_video).delete(admin::delete_video),
        )
        .route(
            "/admin/videos/{video_id}",
            get(admin::admin_get_video).delete(admin::delete_video),
        )
        .route("/videos/{video_id}/status", get(public::video_status))
        .route("/videos/{video_id}/approve", post(upload::approve_video))
        .route(
            "/admin/videos/{video_id}/approve",
            post(upload::approve_video),
        )
        .route(
            "/admin/videos/{video_id}/visibility",
            patch(admin::update_video_visibility),
        )
        .route(
            "/admin/videos/{video_id}/publication",
            patch(admin::update_video_publication),
        )
        .route_layer(default_timeout)
        .route(
            "/videos/upload",
            post(upload::upload_video).layer(upload_timeout),
        )
        .layer(DefaultBodyLimit::disable())
        .layer(cors_layer(config)?)
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
                        service = "rust_admin",
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
        .with_state(state))
}
