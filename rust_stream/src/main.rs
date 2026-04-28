//! Rust Streaming Service
//!
//! Responsibilities:
//!   - Generate HLS manifests from video manifests stored on Autonomi.
//!   - Proxy individual .ts segments by fetching them from the Autonomi network
//!     via the antd daemon REST API.
//!
//! Endpoints:
//!   GET  /health
//!   GET  /stream/{video_id}/{resolution}/playlist.m3u8   → HLS manifest
//!   GET  /stream/{video_id}/{resolution}/{seg_index}.ts  → TS segment bytes

use std::{env, fs, path::PathBuf};

use antd_client::Client as AntdClient;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

// ── Shared state ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    antd: AntdClient,
    catalog_state_path: PathBuf,
    catalog_bootstrap_address: Option<String>,
}

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

#[derive(Deserialize)]
struct CatalogState {
    catalog_address: String,
}

#[derive(Deserialize)]
struct Catalog {
    videos: Vec<CatalogVideo>,
}

#[derive(Deserialize)]
struct CatalogVideo {
    id: String,
    manifest_address: String,
}

#[derive(Deserialize)]
struct VideoManifest {
    id: String,
    status: String,
    variants: Vec<VideoVariant>,
}

#[derive(Deserialize)]
struct VideoVariant {
    resolution: String,
    segment_duration: f64,
    segments: Vec<VideoSegment>,
}

#[derive(Deserialize)]
struct VideoSegment {
    segment_index: i32,
    autonomi_address: String,
    duration: f64,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let antd_url = env::var("ANTD_URL").unwrap_or_else(|_| "http://localhost:8082".into());
    let catalog_state_path = env::var("CATALOG_STATE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/video_catalog/catalog.json"));
    let catalog_bootstrap_address = env::var("CATALOG_ADDRESS")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let antd = AntdClient::new(&antd_url);

    let state = AppState {
        antd,
        catalog_state_path,
        catalog_bootstrap_address,
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health))
        .route(
            "/stream/:video_id/:resolution/playlist.m3u8",
            get(hls_manifest),
        )
        .route(
            "/stream/:video_id/:resolution/:segment_index",
            get(hls_segment),
        )
        .layer(cors)
        .with_state(state);

    let bind_addr = "0.0.0.0:8081";
    info!("Listening on {}", bind_addr);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let autonomi = match state.antd.health().await {
        Ok(status) => AutonomiHealth {
            ok: status.ok,
            network: Some(status.network),
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

/// Serve an HLS playlist (.m3u8) referencing this service's own segment URLs.
async fn hls_manifest(
    State(state): State<AppState>,
    Path((video_id, resolution)): Path<(String, String)>,
) -> Response {
    match build_manifest(&state, &video_id, &resolution).await {
        Ok(manifest) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
            manifest,
        )
            .into_response(),
        Err(e) => {
            error!("manifest error: {e}");
            (StatusCode::NOT_FOUND, e).into_response()
        }
    }
}

/// Proxy a .ts segment from Autonomi to the video player.
async fn hls_segment(
    State(state): State<AppState>,
    Path((video_id, resolution, seg_param)): Path<(String, String, String)>,
) -> Response {
    // Accept both "42" and "42.ts"
    let index_str = seg_param.trim_end_matches(".ts");
    let seg_index: i32 = match index_str.parse() {
        Ok(n) => n,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid segment index").into_response(),
    };

    match fetch_segment(&state, &video_id, &resolution, seg_index).await {
        Ok(bytes) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "video/mp2t")],
            Body::from(bytes),
        )
            .into_response(),
        Err(e) => {
            error!("segment fetch error: {e}");
            (StatusCode::NOT_FOUND, e).into_response()
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn build_manifest(
    state: &AppState,
    video_id: &str,
    resolution: &str,
) -> Result<String, String> {
    let manifest = load_video_manifest(state, video_id).await?;
    if manifest.status != "ready" {
        return Err("video not ready".to_string());
    }

    let variant = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .ok_or_else(|| "variant not found".to_string())?;

    if variant.segments.is_empty() {
        return Err("no segments found".to_string());
    }

    let target_duration = variant
        .segments
        .iter()
        .map(|segment| segment.duration)
        .fold(variant.segment_duration, f64::max)
        .ceil() as u64;
    let mut m3u8 = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:0\n"
    );

    for seg in &variant.segments {
        m3u8.push_str(&format!(
            "#EXTINF:{:.3},\n/stream/{video_id}/{resolution}/{}.ts\n",
            seg.duration, seg.segment_index,
        ));
    }
    m3u8.push_str("#EXT-X-ENDLIST\n");

    Ok(m3u8)
}

async fn fetch_segment(
    state: &AppState,
    video_id: &str,
    resolution: &str,
    seg_index: i32,
) -> Result<Vec<u8>, String> {
    let manifest = load_video_manifest(state, video_id).await?;
    let segment = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .and_then(|variant| {
            variant
                .segments
                .iter()
                .find(|segment| segment.segment_index == seg_index)
        })
        .ok_or_else(|| "segment not found".to_string())?;

    let data = state
        .antd
        .data_get_public(&segment.autonomi_address)
        .await
        .map_err(|e| format!("Autonomi fetch failed: {e}"))?;

    Ok(data)
}

fn read_catalog_address(state: &AppState) -> Option<String> {
    if let Ok(raw) = fs::read_to_string(&state.catalog_state_path) {
        match serde_json::from_str::<CatalogState>(&raw) {
            Ok(catalog_state) if !catalog_state.catalog_address.trim().is_empty() => {
                return Some(catalog_state.catalog_address);
            }
            _ => {}
        }
    }

    state.catalog_bootstrap_address.clone()
}

async fn load_video_manifest(state: &AppState, video_id: &str) -> Result<VideoManifest, String> {
    let catalog_address = read_catalog_address(state)
        .ok_or_else(|| "catalog address not configured".to_string())?;
    let catalog_bytes = state
        .antd
        .data_get_public(&catalog_address)
        .await
        .map_err(|e| format!("Autonomi catalog fetch failed: {e}"))?;
    let catalog: Catalog = serde_json::from_slice(&catalog_bytes)
        .map_err(|e| format!("invalid catalog JSON: {e}"))?;

    let video = catalog
        .videos
        .iter()
        .find(|video| video.id == video_id)
        .ok_or_else(|| "video not found in catalog".to_string())?;

    let manifest_bytes = state
        .antd
        .data_get_public(&video.manifest_address)
        .await
        .map_err(|e| format!("Autonomi manifest fetch failed: {e}"))?;
    let manifest: VideoManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("invalid video manifest JSON: {e}"))?;

    if manifest.id != video_id {
        return Err("video manifest ID mismatch".to_string());
    }

    Ok(manifest)
}
