use std::time::Duration as StdDuration;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{Request, Response, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
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
    let login_attempts = std::sync::Arc::new(LoginThrottle::default());
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
        .route(
            "/auth/login",
            post(login).layer(middleware::from_fn_with_state(
                login_attempts,
                throttle_login,
            )),
        )
        .route("/auth/refresh", post(refresh))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(auth_me))
        .route("/catalog", get(public::get_catalog))
        .route("/videos/upload/quote", post(upload::quote_video_upload))
        .route("/videos", get(public::list_videos))
        .route("/admin/catalogs", get(admin::admin_get_catalogs))
        .route("/admin/catalogs/resume", post(admin::admin_resume_catalogs))
        .route(
            "/admin/catalogs/approve",
            post(admin::admin_approve_catalogs),
        )
        .route(
            "/admin/videos/{video_id}/requote",
            post(upload::requote_video),
        )
        .route(
            "/admin/catalogs/publish",
            post(admin::admin_publish_catalogs),
        )
        .route(
            "/admin/videos/{video_id}/resume",
            post(upload::resume_video),
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
            post(upload::upload_video)
                .layer::<_, std::convert::Infallible>(upload_timeout)
                .layer(DefaultBodyLimit::disable()),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authenticate_request,
        ))
        .layer(middleware::from_fn_with_state(
            autvid_common::native::NativeRequestPolicy::new(
                config.bind_addr,
                config.cors_allowed_origins.clone(),
            ),
            autvid_common::native::enforce_native_request,
        ))
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

async fn authenticate_request(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> axum::response::Response {
    let path = request.uri().path();
    let protected = path.starts_with("/admin/")
        || path.starts_with("/videos/upload")
        || (path.starts_with("/videos/")
            && request.method() != axum::http::Method::GET
            && request.method() != axum::http::Method::HEAD);
    if protected {
        if let Err(error) = crate::auth::require_admin(&state, request.headers()) {
            return error.into_response();
        }
        if !matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
        ) {
            if let Err(error) = crate::auth::require_csrf(request.headers()) {
                return error.into_response();
            }
        }
    }
    next.run(request).await
}

// Bound concurrent authentication and delay bursts of failed attempts without
// turning twenty requests into a minute-long lockout of every valid operator.
struct LoginThrottle {
    failures: std::sync::Mutex<std::collections::VecDeque<std::time::Instant>>,
    slots: tokio::sync::Semaphore,
}
impl Default for LoginThrottle {
    fn default() -> Self {
        Self {
            failures: std::sync::Mutex::new(std::collections::VecDeque::new()),
            slots: tokio::sync::Semaphore::new(32),
        }
    }
}
async fn throttle_login(
    State(throttle): State<std::sync::Arc<LoginThrottle>>,
    request: Request<Body>,
    next: Next,
) -> axum::response::Response {
    let Ok(_slot) = throttle.slots.try_acquire() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, "2")],
            "Authentication capacity busy; retry shortly",
        )
            .into_response();
    };
    let delayed = {
        let mut failures = throttle
            .failures
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let now = std::time::Instant::now();
        while failures
            .front()
            .is_some_and(|t| now.duration_since(*t).as_secs() >= 60)
        {
            failures.pop_front();
        }
        failures.len() >= 20
    };
    if delayed {
        tokio::time::sleep(StdDuration::from_secs(2)).await;
    }
    let response = next.run(request).await;
    if response.status().is_client_error() {
        let mut failures = throttle
            .failures
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if failures.len() == 20 {
            failures.pop_front();
        }
        failures.push_back(std::time::Instant::now());
    }
    response
}

#[cfg(test)]
mod throttle_tests {
    use super::*;
    #[tokio::test]
    async fn failed_login_burst_delays_but_does_not_lock_out_valid_credentials() {
        let throttle = std::sync::Arc::new(LoginThrottle::default());
        let app = Router::new().route(
            "/",
            post(|headers: axum::http::HeaderMap| async move {
                if headers.contains_key("test-valid") {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            })
            .layer(middleware::from_fn_with_state(
                throttle.clone(),
                throttle_login,
            )),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let url = format!("http://{}", listener.local_addr().expect("address"));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("server");
        });
        let client = reqwest::Client::new();
        for _ in 0..20 {
            let response = client.post(&url).send().await.expect("response");
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = client
            .post(&url)
            .header("test-valid", "true")
            .send()
            .await
            .expect("response");
        server.abort();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(throttle.failures.lock().expect("failures").len(), 20);
    }
}
