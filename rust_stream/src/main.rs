//! Rust Streaming Service
//!
//! Responsibilities:
//!   - Generate HLS manifests from video manifests stored on Autonomi.
//!   - Proxy individual .ts segments by fetching them from the Autonomi network
//!     via the antd daemon REST API.
//!
//! Endpoints:
//!   GET  /health
//!   GET  /stream/{video_id}/{resolution}/playlist.m3u8                 → Public HLS manifest
//!   GET  /stream/{video_id}/{resolution}/{seg_index}.ts                → Public TS segment bytes
//!   GET  /stream/manifest/{manifest_address}/{resolution}/playlist.m3u8 → HLS manifest by address
//!   GET  /stream/manifest/{manifest_address}/{resolution}/{seg_index}.ts → TS segment by address

use std::{
    collections::HashMap,
    env, fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use linked_hash_map::LinkedHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

// ── Shared state ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    antd: AntdRestClient,
    catalog_state_path: PathBuf,
    catalog_bootstrap_address: Option<String>,
    cache: Arc<AppCache>,
    cache_config: CacheConfig,
}

#[derive(Clone)]
struct CacheConfig {
    catalog_ttl: Duration,
    manifest_ttl: Duration,
    segment_ttl: Duration,
    segment_max_bytes: usize,
}

struct AppCache {
    catalogs: Mutex<HashMap<String, CachedValue<Catalog>>>,
    manifests: Mutex<HashMap<String, CachedValue<VideoManifest>>>,
    segments: Mutex<SegmentCache>,
    segment_fetches: Mutex<HashMap<String, SegmentFetchReceiver>>,
}

type SegmentFetchResult = Option<Result<Vec<u8>, String>>;
type SegmentFetchReceiver = watch::Receiver<SegmentFetchResult>;

struct CachedValue<T> {
    value: T,
    expires_at: Instant,
}

struct SegmentCache {
    entries: LinkedHashMap<String, CachedSegment>,
    total_bytes: usize,
    max_bytes: usize,
    ttl: Duration,
}

struct CachedSegment {
    data: Vec<u8>,
    expires_at: Instant,
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

#[derive(Clone)]
struct AntdRestClient {
    base_url: String,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct AntdHealthResponse {
    status: String,
    network: Option<String>,
}

#[derive(Deserialize)]
struct AntdPublicDataResponse {
    data: String,
}

fn cors_allowed_origins() -> anyhow::Result<Vec<HeaderValue>> {
    let raw_origins = env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost,http://127.0.0.1".into());
    autvid_common::parse_cors_allowed_origins(&raw_origins)
}

#[derive(Deserialize)]
struct CatalogState {
    catalog_address: Option<String>,
    catalog: Option<Catalog>,
}

#[derive(Clone, Deserialize)]
struct Catalog {
    videos: Vec<CatalogVideo>,
}

#[derive(Clone, Deserialize)]
struct CatalogVideo {
    id: String,
    manifest_address: String,
}

#[derive(Clone, Deserialize)]
struct VideoManifest {
    id: String,
    status: String,
    variants: Vec<VideoVariant>,
}

#[derive(Clone, Deserialize)]
struct VideoVariant {
    resolution: String,
    segment_duration: f64,
    segments: Vec<VideoSegment>,
}

#[derive(Clone, Deserialize)]
struct VideoSegment {
    segment_index: i32,
    autonomi_address: String,
    duration: f64,
}

impl CacheConfig {
    fn from_env() -> Self {
        Self {
            catalog_ttl: duration_from_env("STREAM_CATALOG_CACHE_TTL_SECONDS", 10),
            manifest_ttl: duration_from_env("STREAM_MANIFEST_CACHE_TTL_SECONDS", 300),
            segment_ttl: duration_from_env("STREAM_SEGMENT_CACHE_TTL_SECONDS", 3600),
            segment_max_bytes: usize_from_env("STREAM_SEGMENT_CACHE_MAX_BYTES", 64 * 1024 * 1024),
        }
    }

    fn playlist_max_age_seconds(&self) -> u64 {
        self.catalog_ttl.as_secs()
    }

    fn segment_max_age_seconds(&self) -> u64 {
        self.segment_ttl.as_secs()
    }
}

impl AppCache {
    fn new(config: &CacheConfig) -> Self {
        Self {
            catalogs: Mutex::new(HashMap::new()),
            manifests: Mutex::new(HashMap::new()),
            segments: Mutex::new(SegmentCache::new(
                config.segment_max_bytes,
                config.segment_ttl,
            )),
            segment_fetches: Mutex::new(HashMap::new()),
        }
    }
}

impl SegmentCache {
    fn new(max_bytes: usize, ttl: Duration) -> Self {
        Self {
            entries: LinkedHashMap::new(),
            total_bytes: 0,
            max_bytes,
            ttl,
        }
    }

    fn get(&mut self, address: &str) -> Option<Vec<u8>> {
        if self.disabled() {
            return None;
        }

        let now = Instant::now();
        match self.entries.get_refresh(address) {
            Some(entry) if entry.expires_at > now => {
                let data = entry.data.clone();
                Some(data)
            }
            Some(_) => {
                self.remove(address);
                None
            }
            None => None,
        }
    }

    fn insert(&mut self, address: String, data: Vec<u8>) {
        if self.disabled() || data.len() > self.max_bytes {
            return;
        }

        let now = Instant::now();
        self.remove(&address);
        self.total_bytes += data.len();
        self.entries.insert(
            address,
            CachedSegment {
                data,
                expires_at: now + self.ttl,
            },
        );
        self.evict_expired(now);
        self.evict_to_limit();
    }

    fn disabled(&self) -> bool {
        self.max_bytes == 0 || self.ttl.is_zero()
    }

    fn remove(&mut self, address: &str) {
        if let Some(entry) = self.entries.remove(address) {
            self.total_bytes = self.total_bytes.saturating_sub(entry.data.len());
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        let expired_addresses = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(address, _)| address.to_string())
            .collect::<Vec<_>>();

        for address in expired_addresses {
            self.remove(&address);
        }
    }

    fn evict_to_limit(&mut self) {
        while self.total_bytes > self.max_bytes {
            let Some((_address, entry)) = self.entries.pop_front() else {
                break;
            };
            self.total_bytes = self.total_bytes.saturating_sub(entry.data.len());
        }
    }
}

impl AntdRestClient {
    fn new(base_url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(60))
                .build()?,
        })
    }

    async fn health(&self) -> anyhow::Result<AntdHealthResponse> {
        self.get_json("/health").await
    }

    async fn data_get_public(&self, address: &str) -> anyhow::Result<Vec<u8>> {
        let payload: AntdPublicDataResponse = self
            .get_json(&format!("/v1/data/public/{}", address.trim()))
            .await?;
        BASE64
            .decode(payload.data)
            .map_err(|err| anyhow::anyhow!("antd returned invalid base64 public data: {err}"))
    }

    async fn get_json<T>(&self, path: &str) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self.client.get(&url).send().await?;
        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_else(|_| "".to_string());
            anyhow::bail!("GET {path} failed: {status} {body}");
        }

        response.json::<T>().await.map_err(Into::into)
    }
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
    let cache_config = CacheConfig::from_env();

    let antd = AntdRestClient::new(&antd_url)?;

    let state = AppState {
        antd,
        catalog_state_path,
        catalog_bootstrap_address,
        cache: Arc::new(AppCache::new(&cache_config)),
        cache_config,
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
        ]);

    info!(
        cors_allowed_origins = ?cors_allowed_origins,
        catalog_cache_ttl_seconds = state.cache_config.catalog_ttl.as_secs(),
        manifest_cache_ttl_seconds = state.cache_config.manifest_ttl.as_secs(),
        segment_cache_ttl_seconds = state.cache_config.segment_ttl.as_secs(),
        segment_cache_max_bytes = state.cache_config.segment_max_bytes,
        "configured stream caches"
    );

    let app = Router::new()
        .route("/health", get(health))
        .route("/stream/health", get(health))
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

/// Serve an HLS playlist (.m3u8) referencing this service's own segment URLs.
async fn hls_manifest(
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

/// Serve an HLS playlist directly from a known video manifest address.
async fn hls_manifest_by_address(
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
        Ok(bytes) => (StatusCode::OK, segment_headers(&state), Body::from(bytes)).into_response(),
        Err(e) => {
            error!("segment fetch error: {e}");
            (StatusCode::NOT_FOUND, e).into_response()
        }
    }
}

/// Proxy a .ts segment using a known video manifest address.
async fn hls_segment_by_address(
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn playlist_headers(state: &AppState) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.apple.mpegurl"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        cache_control_header(state.cache_config.playlist_max_age_seconds()),
    );
    headers
}

fn segment_headers(state: &AppState) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t"));
    headers.insert(
        header::CACHE_CONTROL,
        cache_control_header(state.cache_config.segment_max_age_seconds()),
    );
    headers
}

fn cache_control_header(max_age_seconds: u64) -> HeaderValue {
    if max_age_seconds == 0 {
        return HeaderValue::from_static("no-store");
    }

    HeaderValue::from_str(&format!("public, max-age={max_age_seconds}"))
        .unwrap_or_else(|_| HeaderValue::from_static("no-store"))
}

fn duration_from_env(name: &str, default_seconds: u64) -> Duration {
    Duration::from_secs(u64_from_env(name, default_seconds))
}

fn usize_from_env(name: &str, default_value: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default_value)
}

fn u64_from_env(name: &str, default_value: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default_value)
}

async fn build_manifest(
    state: &AppState,
    video_id: &str,
    resolution: &str,
) -> Result<String, String> {
    let manifest = load_video_manifest(state, video_id).await?;
    render_manifest(&manifest, resolution, |segment_index| {
        format!("/stream/{video_id}/{resolution}/{segment_index}.ts")
    })
}

async fn build_manifest_from_address(
    state: &AppState,
    manifest_address: &str,
    resolution: &str,
) -> Result<String, String> {
    let manifest = load_manifest(state, manifest_address).await?;
    render_manifest(&manifest, resolution, |segment_index| {
        format!("/stream/manifest/{manifest_address}/{resolution}/{segment_index}.ts")
    })
}

fn render_manifest<F>(
    manifest: &VideoManifest,
    resolution: &str,
    segment_url: F,
) -> Result<String, String>
where
    F: Fn(i32) -> String,
{
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
            "#EXTINF:{:.3},\n{}\n",
            seg.duration,
            segment_url(seg.segment_index),
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
    let segment_address = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .and_then(|variant| {
            variant
                .segments
                .iter()
                .find(|segment| segment.segment_index == seg_index)
        })
        .map(|segment| segment.autonomi_address.clone())
        .ok_or_else(|| "segment not found".to_string())?;

    fetch_segment_data(state, &segment_address).await
}

async fn fetch_segment_from_address(
    state: &AppState,
    manifest_address: &str,
    resolution: &str,
    seg_index: i32,
) -> Result<Vec<u8>, String> {
    let manifest = load_manifest(state, manifest_address).await?;
    let segment_address = manifest
        .variants
        .iter()
        .find(|variant| variant.resolution == resolution)
        .and_then(|variant| {
            variant
                .segments
                .iter()
                .find(|segment| segment.segment_index == seg_index)
        })
        .map(|segment| segment.autonomi_address.clone())
        .ok_or_else(|| "segment not found".to_string())?;

    fetch_segment_data(state, &segment_address).await
}

fn read_catalog_address(state: &AppState) -> Option<String> {
    if let Ok(raw) = fs::read_to_string(&state.catalog_state_path) {
        if let Ok(catalog_state) = serde_json::from_str::<CatalogState>(&raw) {
            if let Some(address) = catalog_state
                .catalog_address
                .map(|address| address.trim().to_string())
                .filter(|address| !address.is_empty())
            {
                return Some(address);
            }
        }
    }

    state.catalog_bootstrap_address.clone()
}

fn read_catalog_snapshot(state: &AppState) -> Option<Catalog> {
    fs::read_to_string(&state.catalog_state_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<CatalogState>(&raw).ok())
        .and_then(|catalog_state| catalog_state.catalog)
}

async fn load_video_manifest(state: &AppState, video_id: &str) -> Result<VideoManifest, String> {
    let catalog = if let Some(catalog) = read_catalog_snapshot(state) {
        catalog
    } else {
        let catalog_address = read_catalog_address(state)
            .ok_or_else(|| "catalog address not configured".to_string())?;
        load_catalog(state, &catalog_address).await?
    };

    let manifest_address = catalog
        .videos
        .iter()
        .find(|video| video.id == video_id)
        .map(|video| video.manifest_address.clone())
        .ok_or_else(|| "video not found in catalog".to_string())?;

    let manifest = load_manifest(state, &manifest_address).await?;

    if manifest.id != video_id {
        return Err("video manifest ID mismatch".to_string());
    }

    Ok(manifest)
}

async fn load_catalog(state: &AppState, catalog_address: &str) -> Result<Catalog, String> {
    if !state.cache_config.catalog_ttl.is_zero() {
        let now = Instant::now();
        let mut catalogs = state.cache.catalogs.lock().await;
        match catalogs.get(catalog_address) {
            Some(cached) if cached.expires_at > now => return Ok(cached.value.clone()),
            Some(_) => {
                catalogs.remove(catalog_address);
            }
            None => {}
        }
    }

    let catalog_bytes = state
        .antd
        .data_get_public(catalog_address)
        .await
        .map_err(|e| format!("Autonomi catalog fetch failed: {e}"))?;
    let catalog: Catalog =
        serde_json::from_slice(&catalog_bytes).map_err(|e| format!("invalid catalog JSON: {e}"))?;

    if !state.cache_config.catalog_ttl.is_zero() {
        let mut catalogs = state.cache.catalogs.lock().await;
        catalogs.insert(
            catalog_address.to_string(),
            CachedValue {
                value: catalog.clone(),
                expires_at: Instant::now() + state.cache_config.catalog_ttl,
            },
        );
    }

    Ok(catalog)
}

async fn load_manifest(state: &AppState, manifest_address: &str) -> Result<VideoManifest, String> {
    if !state.cache_config.manifest_ttl.is_zero() {
        let now = Instant::now();
        let mut manifests = state.cache.manifests.lock().await;
        match manifests.get(manifest_address) {
            Some(cached) if cached.expires_at > now => return Ok(cached.value.clone()),
            Some(_) => {
                manifests.remove(manifest_address);
            }
            None => {}
        }
    }

    let manifest_bytes = state
        .antd
        .data_get_public(manifest_address)
        .await
        .map_err(|e| format!("Autonomi manifest fetch failed: {e}"))?;
    let manifest: VideoManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("invalid video manifest JSON: {e}"))?;

    if !state.cache_config.manifest_ttl.is_zero() {
        let mut manifests = state.cache.manifests.lock().await;
        manifests.insert(
            manifest_address.to_string(),
            CachedValue {
                value: manifest.clone(),
                expires_at: Instant::now() + state.cache_config.manifest_ttl,
            },
        );
    }

    Ok(manifest)
}

async fn fetch_segment_data(state: &AppState, segment_address: &str) -> Result<Vec<u8>, String> {
    loop {
        {
            let mut segments = state.cache.segments.lock().await;
            if let Some(data) = segments.get(segment_address) {
                return Ok(data);
            }
        }

        let maybe_receiver = {
            let mut fetches = state.cache.segment_fetches.lock().await;
            if let Some(receiver) = fetches.get(segment_address) {
                Some(receiver.clone())
            } else {
                let (sender, receiver) = watch::channel(None);
                fetches.insert(segment_address.to_string(), receiver);
                drop(fetches);

                let result = fetch_segment_data_uncached(state, segment_address).await;
                let _ = sender.send(Some(result.clone()));
                state
                    .cache
                    .segment_fetches
                    .lock()
                    .await
                    .remove(segment_address);
                return result;
            }
        };

        let Some(mut receiver) = maybe_receiver else {
            continue;
        };
        loop {
            let result = receiver.borrow().clone();
            if let Some(result) = result {
                return result;
            }
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
}

async fn fetch_segment_data_uncached(
    state: &AppState,
    segment_address: &str,
) -> Result<Vec<u8>, String> {
    let data = state
        .antd
        .data_get_public(segment_address)
        .await
        .map_err(|e| format!("Autonomi fetch failed: {e}"))?;

    let mut segments = state.cache.segments.lock().await;
    segments.insert(segment_address.to_string(), data.clone());

    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    const TEST_CATALOG_ADDRESS: &str = "test-catalog";
    const TEST_MANIFEST_ADDRESS: &str = "test-manifest";

    fn test_state(catalog_bootstrap_address: Option<&str>) -> AppState {
        let cache_config = CacheConfig {
            catalog_ttl: Duration::from_secs(60),
            manifest_ttl: Duration::from_secs(60),
            segment_ttl: Duration::from_secs(60),
            segment_max_bytes: 1024,
        };

        AppState {
            antd: AntdRestClient::new("http://127.0.0.1:0").unwrap(),
            catalog_state_path: env::temp_dir().join(format!(
                "rust_stream_missing_catalog_{}.json",
                std::process::id()
            )),
            catalog_bootstrap_address: catalog_bootstrap_address.map(str::to_string),
            cache: Arc::new(AppCache::new(&cache_config)),
            cache_config,
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
            }],
        }
    }

    #[test]
    fn segment_cache_evicts_least_recently_used_entries() {
        let mut cache = SegmentCache::new(6, Duration::from_secs(60));

        cache.insert("a".to_string(), vec![1, 2, 3]);
        cache.insert("b".to_string(), vec![4, 5, 6]);
        assert_eq!(cache.get("a"), Some(vec![1, 2, 3]));

        cache.insert("c".to_string(), vec![7, 8, 9]);

        assert_eq!(cache.get("b"), None);
        assert_eq!(cache.get("a"), Some(vec![1, 2, 3]));
        assert_eq!(cache.get("c"), Some(vec![7, 8, 9]));
    }

    #[test]
    fn segment_cache_skips_oversized_and_disabled_entries() {
        let mut cache = SegmentCache::new(3, Duration::from_secs(60));
        cache.insert("too-large".to_string(), vec![1, 2, 3, 4]);
        assert_eq!(cache.get("too-large"), None);

        let mut disabled = SegmentCache::new(0, Duration::from_secs(60));
        disabled.insert("off".to_string(), vec![1]);
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

        let response = hls_manifest(
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
    async fn hls_manifest_by_address_route_renders_manifest_address_segment_urls() {
        let state = test_state(None);
        state.cache.manifests.lock().await.insert(
            TEST_MANIFEST_ADDRESS.to_string(),
            CachedValue {
                value: ready_manifest(),
                expires_at: Instant::now() + Duration::from_secs(60),
            },
        );

        let response = hls_manifest_by_address(
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
        let response = hls_manifest(
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

        let response = hls_manifest(
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
