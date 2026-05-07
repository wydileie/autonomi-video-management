//! Rust Streaming Service
//!
//! Responsibilities:
//! - Generate HLS manifests from video manifests stored on Autonomi.
//! - Proxy individual .ts segments by fetching them from the Autonomi network
//!   via the antd daemon REST API.

use std::{env, path::PathBuf, sync::Arc, time::Duration as StdDuration};

use autvid_common::secret_env;
use axum::http::{header, HeaderName, Method, Request, Response};
use tower_http::{
    cors::CorsLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use tracing::{info, info_span, Span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::antd_client::AntdRestClient;
use crate::cache::AppCache;
use crate::config::{cors_allowed_origins, request_timeout_from_env, CacheConfig};
use crate::state::AppState;

mod antd_client;
mod cache;
mod config;
mod hls;
mod metrics;
mod models;
mod routes;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let antd_url = env::var("ANTD_URL").unwrap_or_else(|_| "http://localhost:8082".into());
    let antd_internal_token = secret_env("ANTD_INTERNAL_TOKEN", "ANTD_INTERNAL_TOKEN_FILE")?;
    let catalog_state_path = env::var("CATALOG_STATE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/video_catalog/catalog.json"));
    let catalog_bootstrap_address = env::var("CATALOG_ADDRESS")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let cache_config = CacheConfig::from_env();
    let request_timeout = request_timeout_from_env();

    let antd = AntdRestClient::new(&antd_url, antd_internal_token)?;

    let state = AppState {
        antd,
        catalog_state_path,
        catalog_bootstrap_address,
        cache: Arc::new(AppCache::new(&cache_config)),
        cache_config,
        metrics: Arc::new(metrics::StreamMetrics::default()),
    };
    let cors_allowed_origins = cors_allowed_origins()?;

    let cors = CorsLayer::new()
        .allow_origin(cors_allowed_origins.clone())
        .allow_methods([Method::GET, Method::HEAD, Method::OPTIONS])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::RANGE,
            HeaderName::from_static("x-request-id"),
        ])
        .expose_headers([HeaderName::from_static("x-request-id")]);

    info!(
        cors_allowed_origins = ?cors_allowed_origins,
        catalog_cache_ttl_seconds = state.cache_config.catalog_ttl.as_secs(),
        manifest_cache_ttl_seconds = state.cache_config.manifest_ttl.as_secs(),
        segment_cache_ttl_seconds = state.cache_config.segment_ttl.as_secs(),
        segment_cache_max_bytes = state.cache_config.segment_max_bytes,
        request_timeout_seconds = request_timeout.as_secs(),
        "configured stream caches"
    );

    let service_metrics = state.metrics.clone();
    let app = routes::router()
        .layer(TimeoutLayer::new(request_timeout))
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
                        service = "rust_stream",
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
        .with_state(state);

    let bind_addr = "0.0.0.0:8081";
    info!("Listening on {}", bind_addr);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
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

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    use axum::body::to_bytes;
    use axum::body::Body;
    use axum::extract::{Path, State};
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    use super::*;
    use crate::cache::{CachedValue, SegmentCache};
    use crate::models::{Catalog, CatalogVideo, VideoManifest, VideoSegment, VideoVariant};

    const TEST_CATALOG_ADDRESS: &str = "test-catalog";
    const TEST_MANIFEST_ADDRESS: &str = "test-manifest";

    fn test_state(catalog_bootstrap_address: Option<&str>) -> AppState {
        test_state_with_antd(catalog_bootstrap_address, "http://127.0.0.1:0")
    }

    fn test_state_with_antd(catalog_bootstrap_address: Option<&str>, antd_url: &str) -> AppState {
        let cache_config = CacheConfig {
            catalog_ttl: Duration::from_secs(60),
            manifest_ttl: Duration::from_secs(60),
            segment_ttl: Duration::from_secs(60),
            segment_max_bytes: 1024,
        };

        AppState {
            antd: AntdRestClient::new(antd_url, None).unwrap(),
            catalog_state_path: env::temp_dir().join(format!(
                "rust_stream_missing_catalog_{}.json",
                std::process::id()
            )),
            catalog_bootstrap_address: catalog_bootstrap_address.map(str::to_string),
            cache: Arc::new(AppCache::new(&cache_config)),
            cache_config,
            metrics: Arc::new(metrics::StreamMetrics::default()),
        }
    }

    async fn cache_catalog_and_manifest(
        state: &AppState,
        catalog_address: &str,
        manifest_address: &str,
        manifest: VideoManifest,
    ) {
        state.cache.catalogs.lock().await.insert(
            catalog_address.to_string(),
            CachedValue {
                value: Catalog {
                    videos: vec![CatalogVideo {
                        id: manifest.id.clone(),
                        manifest_address: manifest_address.to_string(),
                    }],
                },
                expires_at: Instant::now() + Duration::from_secs(60),
            },
        );
        state.cache.manifests.lock().await.insert(
            manifest_address.to_string(),
            CachedValue {
                value: manifest,
                expires_at: Instant::now() + Duration::from_secs(60),
            },
        );
    }

    fn ready_manifest() -> VideoManifest {
        VideoManifest {
            id: "video-1".to_string(),
            status: "ready".to_string(),
            variants: vec![VideoVariant {
                resolution: "720p".to_string(),
                segment_duration: 4.0,
                segments: vec![
                    VideoSegment {
                        segment_index: 0,
                        autonomi_address: "segment-0".to_string(),
                        duration: 3.2,
                    },
                    VideoSegment {
                        segment_index: 1,
                        autonomi_address: "segment-1".to_string(),
                        duration: 4.4,
                    },
                ],
                segments_by_index: Vec::new(),
            }],
        }
    }

    #[derive(Clone, Default)]
    struct MockAntdState {
        catalog_requests: Arc<AtomicUsize>,
        manifest_requests: Arc<AtomicUsize>,
        segment_requests: Arc<AtomicUsize>,
    }

    async fn spawn_stream_mock_antd(state: MockAntdState) -> String {
        let app = Router::new()
            .route("/health", get(mock_health))
            .route(
                "/v1/data/public/:address/raw",
                get(mock_data_get_public_raw),
            )
            .route("/v1/data/public/:address", get(mock_data_get_public))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    async fn mock_health() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "status": "ok",
            "network": "mocknet",
        }))
    }

    async fn mock_data_get_public(
        State(state): State<MockAntdState>,
        Path(address): Path<String>,
    ) -> axum::response::Response<Body> {
        let Some(bytes) = mock_public_bytes(&state, &address).await else {
            return (StatusCode::NOT_FOUND, "unknown address").into_response();
        };

        Json(serde_json::json!({ "data": BASE64.encode(bytes) })).into_response()
    }

    async fn mock_data_get_public_raw(
        State(state): State<MockAntdState>,
        Path(address): Path<String>,
    ) -> axum::response::Response<Body> {
        let Some(bytes) = mock_public_bytes(&state, &address).await else {
            return (StatusCode::NOT_FOUND, "unknown address").into_response();
        };

        ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response()
    }

    async fn mock_public_bytes(state: &MockAntdState, address: &str) -> Option<Vec<u8>> {
        let bytes = match address {
            TEST_CATALOG_ADDRESS => {
                state.catalog_requests.fetch_add(1, Ordering::Relaxed);
                serde_json::to_vec(&serde_json::json!({
                    "videos": [{
                        "id": "video-1",
                        "manifest_address": TEST_MANIFEST_ADDRESS
                    }]
                }))
                .unwrap()
            }
            TEST_MANIFEST_ADDRESS => {
                state.manifest_requests.fetch_add(1, Ordering::Relaxed);
                serde_json::to_vec(&serde_json::json!({
                    "id": "video-1",
                    "status": "ready",
                    "variants": [{
                        "resolution": "720p",
                        "segment_duration": 4.0,
                        "segments": [{
                            "segment_index": 0,
                            "autonomi_address": "segment-0",
                            "duration": 3.0
                        }]
                    }]
                }))
                .unwrap()
            }
            "segment-0" => {
                state.segment_requests.fetch_add(1, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(50)).await;
                b"segment bytes".to_vec()
            }
            _ => return None,
        };

        Some(bytes)
    }

    #[test]
    fn segment_cache_evicts_least_recently_used_entries() {
        let mut cache = SegmentCache::new(6, Duration::from_secs(60));

        cache.insert("a".to_string(), bytes::Bytes::from_static(&[1, 2, 3]));
        cache.insert("b".to_string(), bytes::Bytes::from_static(&[4, 5, 6]));
        assert_eq!(cache.get("a"), Some(bytes::Bytes::from_static(&[1, 2, 3])));

        cache.insert("c".to_string(), bytes::Bytes::from_static(&[7, 8, 9]));

        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("a"), Some(bytes::Bytes::from_static(&[1, 2, 3])));
        assert_eq!(cache.get("c"), Some(bytes::Bytes::from_static(&[7, 8, 9])));
        let snapshot = cache.snapshot();
        assert_eq!(snapshot.evictions_total, 1);
        assert_eq!(snapshot.bytes_resident, 6);
        assert_eq!(snapshot.entries, 2);
    }

    #[test]
    fn segment_cache_skips_oversized_and_disabled_entries() {
        let mut cache = SegmentCache::new(3, Duration::from_secs(60));
        cache.insert(
            "too-large".to_string(),
            bytes::Bytes::from_static(&[1, 2, 3, 4]),
        );
        assert_eq!(cache.get("too-large"), None);

        let mut disabled = SegmentCache::new(0, Duration::from_secs(60));
        disabled.insert("off".to_string(), bytes::Bytes::from_static(&[1]));
        assert_eq!(disabled.get("off"), None);
    }

    #[tokio::test]
    async fn hls_manifest_route_renders_cached_playlist_with_headers() {
        let state = test_state(Some(TEST_CATALOG_ADDRESS));
        cache_catalog_and_manifest(
            &state,
            TEST_CATALOG_ADDRESS,
            TEST_MANIFEST_ADDRESS,
            ready_manifest(),
        )
        .await;

        let response = routes::hls_manifest(
            State(state),
            Path(("video-1".to_string(), "720p".to_string())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/vnd.apple.mpegurl"
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=60"
        );

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            body.as_ref(),
            b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:3.200,\n/stream/video-1/720p/0.ts\n#EXTINF:4.400,\n/stream/video-1/720p/1.ts\n#EXT-X-ENDLIST\n"
        );
    }

    #[tokio::test]
    async fn hls_routes_fetch_catalog_manifest_and_segment_from_mock_antd() {
        let mock_state = MockAntdState::default();
        let base_url = spawn_stream_mock_antd(mock_state.clone()).await;
        let state = test_state_with_antd(Some(TEST_CATALOG_ADDRESS), &base_url);

        let playlist = routes::hls_manifest(
            State(state.clone()),
            Path(("video-1".to_string(), "720p".to_string())),
        )
        .await;
        assert_eq!(playlist.status(), StatusCode::OK);
        let body = to_bytes(playlist.into_body(), usize::MAX).await.unwrap();
        assert!(std::str::from_utf8(body.as_ref())
            .unwrap()
            .contains("/stream/video-1/720p/0.ts"));

        let first_segment = routes::hls_segment(
            State(state.clone()),
            Path((
                "video-1".to_string(),
                "720p".to_string(),
                "0.ts".to_string(),
            )),
        )
        .await;
        assert_eq!(first_segment.status(), StatusCode::OK);
        assert_eq!(
            first_segment.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=60, immutable"
        );
        let first_body = to_bytes(first_segment.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(first_body.as_ref(), b"segment bytes");

        let cached_segment = routes::hls_segment(
            State(state),
            Path((
                "video-1".to_string(),
                "720p".to_string(),
                "0.ts".to_string(),
            )),
        )
        .await;
        assert_eq!(cached_segment.status(), StatusCode::OK);

        assert_eq!(mock_state.catalog_requests.load(Ordering::Relaxed), 1);
        assert_eq!(mock_state.manifest_requests.load(Ordering::Relaxed), 1);
        assert_eq!(mock_state.segment_requests.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn concurrent_segment_misses_are_coalesced_against_mock_antd() {
        let mock_state = MockAntdState::default();
        let base_url = spawn_stream_mock_antd(mock_state.clone()).await;
        let state = test_state_with_antd(Some(TEST_CATALOG_ADDRESS), &base_url);
        cache_catalog_and_manifest(
            &state,
            TEST_CATALOG_ADDRESS,
            TEST_MANIFEST_ADDRESS,
            ready_manifest(),
        )
        .await;

        let first = routes::hls_segment(
            State(state.clone()),
            Path((
                "video-1".to_string(),
                "720p".to_string(),
                "0.ts".to_string(),
            )),
        );
        let second = routes::hls_segment(
            State(state.clone()),
            Path((
                "video-1".to_string(),
                "720p".to_string(),
                "0.ts".to_string(),
            )),
        );
        let (first, second) = tokio::join!(first, second);

        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        assert_eq!(mock_state.segment_requests.load(Ordering::Relaxed), 1);
        assert!(state
            .metrics
            .render_prometheus_with_cache(None)
            .contains("autvid_stream_segment_fetch_coalesced_total{service=\"rust_stream\"} 1"));
    }

    #[tokio::test]
    async fn hls_manifest_by_address_route_renders_manifest_address_segment_urls() {
        let state = test_state(None);
        state.cache.manifests.lock().await.insert(
            TEST_MANIFEST_ADDRESS.to_string(),
            CachedValue {
                value: ready_manifest(),
                expires_at: Instant::now() + Duration::from_secs(60),
            },
        );

        let response = routes::hls_manifest_by_address(
            State(state),
            Path((TEST_MANIFEST_ADDRESS.to_string(), "720p".to_string())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            body.as_ref(),
            b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:5\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:3.200,\n/stream/manifest/test-manifest/720p/0.ts\n#EXTINF:4.400,\n/stream/manifest/test-manifest/720p/1.ts\n#EXT-X-ENDLIST\n"
        );
    }

    #[tokio::test]
    async fn hls_manifest_route_returns_not_found_when_catalog_address_missing() {
        let response = routes::hls_manifest(
            State(test_state(None)),
            Path(("video-1".to_string(), "720p".to_string())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"catalog address not configured");
    }

    #[tokio::test]
    async fn hls_manifest_route_returns_not_found_when_video_not_ready() {
        let state = test_state(Some(TEST_CATALOG_ADDRESS));
        let mut manifest = ready_manifest();
        manifest.status = "processing".to_string();
        cache_catalog_and_manifest(
            &state,
            TEST_CATALOG_ADDRESS,
            TEST_MANIFEST_ADDRESS,
            manifest,
        )
        .await;

        let response = routes::hls_manifest(
            State(state),
            Path(("video-1".to_string(), "720p".to_string())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"video not ready");
    }

    #[test]
    fn cors_origin_normalization_accepts_explicit_origins() {
        assert_eq!(
            autvid_common::normalize_cors_origin(" https://example.com/ ").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            autvid_common::normalize_cors_origin("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn cors_origin_normalization_rejects_wildcards_paths_and_missing_schemes() {
        assert!(autvid_common::normalize_cors_origin("*").is_err());
        assert!(autvid_common::normalize_cors_origin("https://example.com/app").is_err());
        assert!(autvid_common::normalize_cors_origin("example.com").is_err());
    }
}
