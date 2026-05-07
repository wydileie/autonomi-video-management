use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use tracing::error;

use crate::hls::{
    build_manifest, build_manifest_from_address, fetch_segment, fetch_segment_from_address,
    playlist_headers, segment_headers,
};
use crate::state::AppState;

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    autonomi: AutonomiHealth,
}

#[derive(Serialize)]
struct AutonomiHealth {
    ok: bool,
    network: Option<String>,
    error: Option<String>,
}

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/livez", get(livez))
        .route("/stream/livez", get(livez))
        .route("/health", get(health))
        .route("/stream/health", get(health))
        .route("/metrics", get(metrics))
        .route("/stream/metrics", get(metrics))
        .route(
            "/stream/manifest/:manifest_address/:resolution/playlist.m3u8",
            get(hls_manifest_by_address),
        )
        .route(
            "/stream/manifest/:manifest_address/:resolution/:segment_index",
            get(hls_segment_by_address),
        )
        .route(
            "/stream/:video_id/:resolution/playlist.m3u8",
            get(hls_manifest),
        )
        .route(
            "/stream/:video_id/:resolution/:segment_index",
            get(hls_segment),
        )
}

async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let segment_cache = state.cache.segments.lock().await.snapshot();
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state
            .metrics
            .render_prometheus_with_cache(Some(segment_cache)),
    )
}

async fn livez() -> impl IntoResponse {
    StatusCode::OK
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let autonomi = match state.antd.health().await {
        Ok(status) => AutonomiHealth {
            ok: status.status.eq_ignore_ascii_case("ok"),
            network: status.network,
            error: None,
        },
        Err(err) => AutonomiHealth {
            ok: false,
            network: None,
            error: Some(err.to_string()),
        },
    };

    let ok = autonomi.ok;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status, axum::Json(HealthResponse { ok, autonomi }))
}

pub(crate) async fn hls_manifest(
    State(state): State<AppState>,
    Path((video_id, resolution)): Path<(String, String)>,
) -> Response {
    match build_manifest(&state, &video_id, &resolution).await {
        Ok(manifest) => (StatusCode::OK, playlist_headers(&state), manifest).into_response(),
        Err(e) => {
            error!("manifest error: {e}");
            (StatusCode::NOT_FOUND, e).into_response()
        }
    }
}

pub(crate) async fn hls_manifest_by_address(
    State(state): State<AppState>,
    Path((manifest_address, resolution)): Path<(String, String)>,
) -> Response {
    match build_manifest_from_address(&state, &manifest_address, &resolution).await {
        Ok(manifest) => (StatusCode::OK, playlist_headers(&state), manifest).into_response(),
        Err(e) => {
            error!("manifest-by-address error: {e}");
            (StatusCode::NOT_FOUND, e).into_response()
        }
    }
}

pub(crate) async fn hls_segment(
    State(state): State<AppState>,
    Path((video_id, resolution, seg_param)): Path<(String, String, String)>,
) -> Response {
    let index_str = seg_param.trim_end_matches(".ts");
    let seg_index: i32 = match index_str.parse() {
        Ok(n) => n,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid segment index").into_response(),
    };

    match fetch_segment(&state, &video_id, &resolution, seg_index).await {
        Ok(bytes) => (StatusCode::OK, segment_headers(&state), Body::from(bytes)).into_response(),
        Err(e) => {
            error!("segment fetch error: {e}");
            (StatusCode::NOT_FOUND, e).into_response()
        }
    }
}

pub(crate) async fn hls_segment_by_address(
    State(state): State<AppState>,
    Path((manifest_address, resolution, seg_param)): Path<(String, String, String)>,
) -> Response {
    let index_str = seg_param.trim_end_matches(".ts");
    let seg_index: i32 = match index_str.parse() {
        Ok(n) => n,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid segment index").into_response(),
    };

    match fetch_segment_from_address(&state, &manifest_address, &resolution, seg_index).await {
        Ok(bytes) => (StatusCode::OK, segment_headers(&state), Body::from(bytes)).into_response(),
        Err(e) => {
            error!("segment-by-address fetch error: {e}");
            (StatusCode::NOT_FOUND, e).into_response()
        }
    }
}
