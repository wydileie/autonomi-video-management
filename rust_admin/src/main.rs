use std::{
    collections::HashMap,
    env, fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration as StdDuration, Instant},
};

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use subtle::ConstantTimeEq;
use tokio::{
    fs as tokio_fs,
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
    sync::{Mutex, Semaphore},
    task::JoinSet,
    time::sleep,
};
use tokio_util::io::ReaderStream;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use uuid::Uuid;

const STATUS_PENDING: &str = "pending";
const STATUS_PROCESSING: &str = "processing";
const STATUS_AWAITING_APPROVAL: &str = "awaiting_approval";
const STATUS_UPLOADING: &str = "uploading";
const STATUS_READY: &str = "ready";
const STATUS_ERROR: &str = "error";
const DEFAULT_API_PORT: u16 = 8000;
const CATALOG_CONTENT_TYPE: &str = "application/vnd.autonomi.video.catalog+json;v=1";
const VIDEO_MANIFEST_CONTENT_TYPE: &str = "application/vnd.autonomi.video.manifest+json;v=1";
const MIN_ANTD_SELF_ENCRYPTION_BYTES: usize = 3;
const JOB_KIND_PROCESS_VIDEO: &str = "process_video";
const JOB_KIND_UPLOAD_VIDEO: &str = "upload_video";
const JOB_KIND_PUBLISH_CATALOG: &str = "publish_catalog";
const JOB_STATUS_QUEUED: &str = "queued";
const JOB_STATUS_RUNNING: &str = "running";
const JOB_STATUS_SUCCEEDED: &str = "succeeded";
const JOB_STATUS_FAILED: &str = "failed";
const SUPPORTED_RESOLUTIONS: &[&str] = &[
    "8k", "4k", "1440p", "1080p", "720p", "540p", "480p", "360p", "240p", "144p",
];

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    pool: PgPool,
    antd: AntdRestClient,
    catalog_lock: Arc<Mutex<()>>,
    catalog_publish_lock: Arc<Mutex<()>>,
    catalog_publish_epoch: Arc<AtomicU64>,
    upload_save_semaphore: Arc<Semaphore>,
}

#[derive(Clone)]
struct Config {
    db_dsn: String,
    antd_url: String,
    antd_payment_mode: String,
    antd_metadata_payment_mode: String,
    admin_username: String,
    admin_password: String,
    admin_auth_secret: String,
    admin_auth_ttl_hours: i64,
    catalog_state_path: PathBuf,
    catalog_bootstrap_address: Option<String>,
    cors_allowed_origins: Vec<HeaderValue>,
    bind_addr: SocketAddr,
    upload_temp_dir: PathBuf,
    upload_max_file_bytes: u64,
    upload_min_free_bytes: u64,
    upload_max_concurrent_saves: usize,
    upload_ffprobe_timeout_seconds: f64,
    hls_segment_duration: f64,
    ffmpeg_threads: usize,
    ffmpeg_filter_threads: usize,
    upload_max_duration_seconds: f64,
    upload_max_source_pixels: i64,
    upload_max_source_long_edge: i64,
    upload_quote_transcoded_overhead: f64,
    upload_quote_max_sample_bytes: usize,
    final_quote_approval_ttl_seconds: i64,
    approval_cleanup_interval_seconds: u64,
    antd_upload_verify: bool,
    antd_upload_retries: usize,
    antd_upload_timeout_seconds: f64,
    antd_quote_concurrency: usize,
    antd_upload_concurrency: usize,
    antd_approve_on_startup: bool,
    antd_require_cost_ready: bool,
    antd_direct_upload_max_bytes: usize,
    admin_job_workers: usize,
    admin_job_poll_interval_seconds: u64,
    admin_job_lease_seconds: i64,
    admin_job_max_attempts: i32,
    catalog_publish_job_max_attempts: i32,
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

#[derive(Deserialize)]
struct AntdDataCostResponse {
    cost: Option<String>,
    chunk_count: Option<i64>,
    estimated_gas_cost_wei: Option<String>,
    payment_mode: Option<String>,
}

#[derive(Deserialize)]
struct AntdDataPutResponse {
    address: String,
    cost: Option<String>,
}

#[derive(Deserialize)]
struct AntdFilePutResponse {
    address: String,
    byte_size: u64,
    storage_cost_atto: String,
    payment_mode_used: String,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    autonomi: AutonomiHealth,
    postgres: PostgresHealth,
    write_ready: bool,
    payment_mode: String,
    final_quote_approval_ttl_seconds: i64,
    implementation: &'static str,
    role: &'static str,
}

#[derive(Serialize)]
struct AutonomiHealth {
    ok: bool,
    network: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct PostgresHealth {
    ok: bool,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct AuthTokenOut {
    access_token: String,
    token_type: &'static str,
    expires_at: String,
    username: String,
}

#[derive(Serialize)]
struct AdminMeOut {
    username: String,
}

#[derive(Deserialize)]
struct VideoVisibilityUpdate {
    #[serde(default, rename = "show_original_filename")]
    _show_original_filename: bool,
    show_manifest_address: bool,
}

#[derive(Deserialize)]
struct VideoPublicationUpdate {
    is_public: bool,
}

#[derive(Deserialize)]
struct UploadQuoteRequest {
    duration_seconds: f64,
    resolutions: Vec<String>,
    source_width: Option<i32>,
    source_height: Option<i32>,
    #[serde(default)]
    upload_original: bool,
    source_size_bytes: Option<i64>,
}

#[derive(Serialize, Clone)]
struct SegmentOut {
    segment_index: i32,
    autonomi_address: Option<String>,
    duration: f64,
}

#[derive(Serialize, Clone)]
struct VariantOut {
    id: String,
    resolution: String,
    width: i32,
    height: i32,
    total_duration: Option<f64>,
    segment_count: Option<i32>,
    segments: Vec<SegmentOut>,
}

#[derive(Serialize, Clone)]
struct VideoOut {
    id: String,
    title: String,
    original_filename: Option<String>,
    description: Option<String>,
    status: String,
    created_at: String,
    manifest_address: Option<String>,
    catalog_address: Option<String>,
    is_public: bool,
    show_original_filename: bool,
    show_manifest_address: bool,
    upload_original: bool,
    original_file_address: Option<String>,
    original_file_byte_size: Option<i64>,
    publish_when_ready: bool,
    error_message: Option<String>,
    final_quote: Option<Value>,
    final_quote_created_at: Option<String>,
    approval_expires_at: Option<String>,
    variants: Vec<VariantOut>,
}

struct CatalogEntryInput {
    video_id: String,
    title: String,
    description: Option<String>,
    created_at: String,
    updated_at: String,
    manifest_address: String,
    show_manifest_address: bool,
    variants: Vec<Value>,
}

#[derive(Serialize)]
struct UploadQuoteVariantOut {
    resolution: String,
    width: i32,
    height: i32,
    segment_count: i64,
    estimated_bytes: i64,
    chunk_count: i64,
    storage_cost_atto: String,
    estimated_gas_cost_wei: String,
    payment_mode: String,
}

#[derive(Serialize)]
struct UploadQuoteOriginalOut {
    estimated_bytes: i64,
    chunk_count: i64,
    storage_cost_atto: String,
    estimated_gas_cost_wei: String,
    payment_mode: String,
}

#[derive(Serialize)]
struct UploadQuoteOut {
    duration_seconds: f64,
    segment_duration: f64,
    payment_mode: String,
    estimated_bytes: i64,
    segment_count: i64,
    storage_cost_atto: String,
    estimated_gas_cost_wei: String,
    metadata_bytes: i64,
    sampled: bool,
    original_file: Option<UploadQuoteOriginalOut>,
    variants: Vec<UploadQuoteVariantOut>,
}

struct AcceptedUpload {
    video_id: String,
    video: VideoOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JobKind {
    ProcessVideo,
    UploadVideo,
    PublishCatalog,
}

impl JobKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ProcessVideo => JOB_KIND_PROCESS_VIDEO,
            Self::UploadVideo => JOB_KIND_UPLOAD_VIDEO,
            Self::PublishCatalog => JOB_KIND_PUBLISH_CATALOG,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            JOB_KIND_PROCESS_VIDEO => Some(Self::ProcessVideo),
            JOB_KIND_UPLOAD_VIDEO => Some(Self::UploadVideo),
            JOB_KIND_PUBLISH_CATALOG => Some(Self::PublishCatalog),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
struct LeasedJob {
    id: Uuid,
    kind: JobKind,
    video_id: Option<Uuid>,
    attempts: i32,
    max_attempts: i32,
}

struct UploadMediaMetadata {
    duration_seconds: f64,
    dimensions: (i32, i32),
}

struct CommandOutput {
    status_code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct JobDirGuard {
    path: PathBuf,
    armed: bool,
}

impl JobDirGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for JobDirGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    detail: String,
    authenticate: bool,
}

impl ApiError {
    fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
            authenticate: false,
        }
    }

    fn unauthorized(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            detail: detail.into(),
            authenticate: true,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({ "detail": self.detail }))).into_response();
        if self.authenticate {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = Arc::new(Config::from_env()?);
    let pool = PgPoolOptions::new()
        .min_connections(2)
        .max_connections(10)
        .connect(&config.db_dsn)
        .await?;
    ensure_schema(&pool).await?;

    fs::create_dir_all(&config.upload_temp_dir)?;
    if let Some(parent) = config.catalog_state_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let antd = AntdRestClient::new(
        &config.antd_url,
        config.antd_upload_timeout_seconds.max(60.0) + 30.0,
    )?;
    ensure_autonomi_ready(&config, &antd).await?;

    let state = AppState {
        config: config.clone(),
        pool,
        antd,
        catalog_lock: Arc::new(Mutex::new(())),
        catalog_publish_lock: Arc::new(Mutex::new(())),
        catalog_publish_epoch: Arc::new(AtomicU64::new(0)),
        upload_save_semaphore: Arc::new(Semaphore::new(config.upload_max_concurrent_saves)),
    };
    cleanup_expired_approvals(&state).await?;
    recover_interrupted_jobs(state.clone()).await?;
    start_job_workers(&state);
    tokio::spawn(approval_cleanup_loop(state.clone()));

    let app = Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(login))
        .route("/auth/me", get(auth_me))
        .route("/catalog", get(get_catalog))
        .route("/videos/upload/quote", post(quote_video_upload))
        .route("/videos/upload", post(upload_video))
        .route("/videos", get(list_videos))
        .route("/admin/videos", get(admin_list_videos))
        .route("/videos/:video_id", get(get_video).delete(delete_video))
        .route(
            "/admin/videos/:video_id",
            get(admin_get_video).delete(delete_video),
        )
        .route("/videos/:video_id/status", get(video_status))
        .route("/videos/:video_id/approve", post(approve_video))
        .route("/admin/videos/:video_id/approve", post(approve_video))
        .route(
            "/admin/videos/:video_id/visibility",
            patch(update_video_visibility),
        )
        .route(
            "/admin/videos/:video_id/publication",
            patch(update_video_publication),
        )
        .layer(DefaultBodyLimit::disable())
        .layer(cors_layer(&config)?)
        .with_state(state);

    let listener = TcpListener::bind(config.bind_addr).await?;
    info!("rust_admin listening on {}", config.bind_addr);
    axum::serve(listener, app).await?;
    Ok(())
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        let db_user = required_env("ADMIN_DB_USER")?;
        let db_pass = required_env("ADMIN_DB_PASS")?;
        let db_host = required_env("ADMIN_DB_HOST")?;
        let db_name = required_env("ADMIN_DB_NAME")?;
        let db_port = env::var("ADMIN_DB_PORT").unwrap_or_else(|_| "5432".into());
        let db_dsn = format!("postgresql://{db_user}:{db_pass}@{db_host}:{db_port}/{db_name}");

        let bind_port = env::var("RUST_ADMIN_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(DEFAULT_API_PORT);
        let bind_addr = SocketAddr::from(([0, 0, 0, 0], bind_port));

        let admin_username = env::var("ADMIN_USERNAME").unwrap_or_else(|_| "admin".into());
        let admin_password = env::var("ADMIN_PASSWORD").unwrap_or_else(|_| "admin".into());
        let admin_auth_secret =
            env::var("ADMIN_AUTH_SECRET").unwrap_or_else(|_| admin_password.clone());
        let admin_auth_ttl_hours = parse_i64_env("ADMIN_AUTH_TTL_HOURS", 12)?;
        if admin_auth_ttl_hours <= 0 {
            anyhow::bail!("ADMIN_AUTH_TTL_HOURS must be greater than zero");
        }
        validate_admin_auth_config(
            &admin_username,
            &admin_password,
            &admin_auth_secret,
            admin_auth_ttl_hours,
        )?;

        let antd_payment_mode = env::var("ANTD_PAYMENT_MODE").unwrap_or_else(|_| "auto".into());
        if !matches!(antd_payment_mode.as_str(), "auto" | "merkle" | "single") {
            anyhow::bail!("ANTD_PAYMENT_MODE must be one of auto, merkle, single");
        }
        let antd_metadata_payment_mode =
            env::var("ANTD_METADATA_PAYMENT_MODE").unwrap_or_else(|_| "merkle".into());
        if !matches!(
            antd_metadata_payment_mode.as_str(),
            "auto" | "merkle" | "single"
        ) {
            anyhow::bail!("ANTD_METADATA_PAYMENT_MODE must be one of auto, merkle, single");
        }

        let hls_segment_duration = parse_f64_env("HLS_SEGMENT_DURATION", 1.0)?;
        if hls_segment_duration <= 0.0 {
            anyhow::bail!("HLS_SEGMENT_DURATION must be greater than zero");
        }

        let ffmpeg_threads = parse_usize_env("FFMPEG_THREADS", 2)?;
        if ffmpeg_threads < 1 {
            anyhow::bail!("FFMPEG_THREADS must be at least 1");
        }
        let ffmpeg_filter_threads = parse_usize_env("FFMPEG_FILTER_THREADS", 1)?;
        if ffmpeg_filter_threads < 1 {
            anyhow::bail!("FFMPEG_FILTER_THREADS must be at least 1");
        }

        let upload_quote_transcoded_overhead =
            parse_f64_env("UPLOAD_QUOTE_TRANSCODED_OVERHEAD", 1.08)?;
        if upload_quote_transcoded_overhead < 1.0 {
            anyhow::bail!("UPLOAD_QUOTE_TRANSCODED_OVERHEAD must be at least 1");
        }

        let upload_quote_max_sample_bytes =
            parse_usize_env("UPLOAD_QUOTE_MAX_SAMPLE_BYTES", 16 * 1024 * 1024)?;
        if upload_quote_max_sample_bytes < 1 {
            anyhow::bail!("UPLOAD_QUOTE_MAX_SAMPLE_BYTES must be at least 1");
        }

        let upload_max_file_bytes =
            parse_u64_env("UPLOAD_MAX_FILE_BYTES", 20 * 1024 * 1024 * 1024)?;
        if upload_max_file_bytes == 0 {
            anyhow::bail!("UPLOAD_MAX_FILE_BYTES must be greater than zero");
        }
        let upload_min_free_bytes = parse_u64_env("UPLOAD_MIN_FREE_BYTES", 5 * 1024 * 1024 * 1024)?;
        let upload_max_concurrent_saves = parse_usize_env("UPLOAD_MAX_CONCURRENT_SAVES", 2)?;
        if upload_max_concurrent_saves < 1 {
            anyhow::bail!("UPLOAD_MAX_CONCURRENT_SAVES must be at least 1");
        }
        let upload_ffprobe_timeout_seconds = parse_f64_env("UPLOAD_FFPROBE_TIMEOUT_SECONDS", 30.0)?;
        if upload_ffprobe_timeout_seconds <= 0.0 {
            anyhow::bail!("UPLOAD_FFPROBE_TIMEOUT_SECONDS must be greater than zero");
        }
        let upload_max_duration_seconds =
            parse_f64_env("UPLOAD_MAX_DURATION_SECONDS", 4.0 * 60.0 * 60.0)?;
        if upload_max_duration_seconds <= 0.0 {
            anyhow::bail!("UPLOAD_MAX_DURATION_SECONDS must be greater than zero");
        }
        let upload_max_source_pixels = parse_i64_env("UPLOAD_MAX_SOURCE_PIXELS", 7680 * 4320)?;
        if upload_max_source_pixels <= 0 {
            anyhow::bail!("UPLOAD_MAX_SOURCE_PIXELS must be greater than zero");
        }
        let upload_max_source_long_edge = parse_i64_env("UPLOAD_MAX_SOURCE_LONG_EDGE", 7680)?;
        if upload_max_source_long_edge <= 0 {
            anyhow::bail!("UPLOAD_MAX_SOURCE_LONG_EDGE must be greater than zero");
        }
        let final_quote_approval_ttl_seconds =
            parse_i64_env("FINAL_QUOTE_APPROVAL_TTL_SECONDS", 4 * 60 * 60)?;
        if final_quote_approval_ttl_seconds <= 0 {
            anyhow::bail!("FINAL_QUOTE_APPROVAL_TTL_SECONDS must be greater than zero");
        }
        let approval_cleanup_interval_seconds =
            parse_u64_env("APPROVAL_CLEANUP_INTERVAL_SECONDS", 300)?;
        if approval_cleanup_interval_seconds == 0 {
            anyhow::bail!("APPROVAL_CLEANUP_INTERVAL_SECONDS must be greater than zero");
        }
        let antd_upload_retries = parse_usize_env("ANTD_UPLOAD_RETRIES", 3)?;
        if antd_upload_retries < 1 {
            anyhow::bail!("ANTD_UPLOAD_RETRIES must be at least 1");
        }
        let antd_upload_timeout_seconds = parse_f64_env("ANTD_UPLOAD_TIMEOUT_SECONDS", 120.0)?;
        if antd_upload_timeout_seconds <= 0.0 {
            anyhow::bail!("ANTD_UPLOAD_TIMEOUT_SECONDS must be greater than zero");
        }
        let antd_quote_concurrency = parse_usize_env("ANTD_QUOTE_CONCURRENCY", 8)?;
        if antd_quote_concurrency < 1 {
            anyhow::bail!("ANTD_QUOTE_CONCURRENCY must be at least 1");
        }
        let antd_upload_concurrency = parse_usize_env("ANTD_UPLOAD_CONCURRENCY", 4)?;
        if antd_upload_concurrency < 1 {
            anyhow::bail!("ANTD_UPLOAD_CONCURRENCY must be at least 1");
        }
        let antd_direct_upload_max_bytes =
            parse_usize_env("ANTD_DIRECT_UPLOAD_MAX_BYTES", 16 * 1024 * 1024)?;
        if antd_direct_upload_max_bytes < MIN_ANTD_SELF_ENCRYPTION_BYTES {
            anyhow::bail!("ANTD_DIRECT_UPLOAD_MAX_BYTES must be at least 3");
        }
        let admin_job_workers = parse_usize_env("ADMIN_JOB_WORKERS", 1)?;
        if admin_job_workers < 1 {
            anyhow::bail!("ADMIN_JOB_WORKERS must be at least 1");
        }
        let admin_job_poll_interval_seconds = parse_u64_env("ADMIN_JOB_POLL_INTERVAL_SECONDS", 2)?;
        if admin_job_poll_interval_seconds == 0 {
            anyhow::bail!("ADMIN_JOB_POLL_INTERVAL_SECONDS must be greater than zero");
        }
        let admin_job_lease_seconds = parse_i64_env("ADMIN_JOB_LEASE_SECONDS", 12 * 60 * 60)?;
        if admin_job_lease_seconds <= 0 {
            anyhow::bail!("ADMIN_JOB_LEASE_SECONDS must be greater than zero");
        }
        let admin_job_max_attempts = parse_i32_env("ADMIN_JOB_MAX_ATTEMPTS", 3)?;
        if admin_job_max_attempts < 1 {
            anyhow::bail!("ADMIN_JOB_MAX_ATTEMPTS must be at least 1");
        }
        let catalog_publish_job_max_attempts =
            parse_i32_env("CATALOG_PUBLISH_JOB_MAX_ATTEMPTS", 12)?;
        if catalog_publish_job_max_attempts < 1 {
            anyhow::bail!("CATALOG_PUBLISH_JOB_MAX_ATTEMPTS must be at least 1");
        }

        Ok(Self {
            db_dsn,
            antd_url: env::var("ANTD_URL").unwrap_or_else(|_| "http://localhost:8082".into()),
            antd_payment_mode,
            antd_metadata_payment_mode,
            admin_username,
            admin_password,
            admin_auth_secret,
            admin_auth_ttl_hours,
            catalog_state_path: PathBuf::from(
                env::var("CATALOG_STATE_PATH")
                    .unwrap_or_else(|_| "/tmp/video_catalog/catalog.json".into()),
            ),
            catalog_bootstrap_address: env::var("CATALOG_ADDRESS")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            cors_allowed_origins: cors_allowed_origins()?,
            bind_addr,
            upload_temp_dir: PathBuf::from(
                env::var("UPLOAD_TEMP_DIR").unwrap_or_else(|_| "/tmp/video_uploads".into()),
            ),
            upload_max_file_bytes,
            upload_min_free_bytes,
            upload_max_concurrent_saves,
            upload_ffprobe_timeout_seconds,
            hls_segment_duration,
            ffmpeg_threads,
            ffmpeg_filter_threads,
            upload_max_duration_seconds,
            upload_max_source_pixels,
            upload_max_source_long_edge,
            upload_quote_transcoded_overhead,
            upload_quote_max_sample_bytes,
            final_quote_approval_ttl_seconds,
            approval_cleanup_interval_seconds,
            antd_upload_verify: parse_bool_env("ANTD_UPLOAD_VERIFY", true),
            antd_upload_retries,
            antd_upload_timeout_seconds,
            antd_quote_concurrency,
            antd_upload_concurrency,
            antd_approve_on_startup: parse_bool_env("ANTD_APPROVE_ON_STARTUP", true),
            antd_require_cost_ready: parse_bool_env("ANTD_REQUIRE_COST_READY", false),
            antd_direct_upload_max_bytes,
            admin_job_workers,
            admin_job_poll_interval_seconds,
            admin_job_lease_seconds,
            admin_job_max_attempts,
            catalog_publish_job_max_attempts,
        })
    }
}

fn required_env(name: &str) -> anyhow::Result<String> {
    env::var(name).map_err(|_| anyhow::anyhow!("{name} is required"))
}

fn parse_i64_env(name: &str, default_value: i64) -> anyhow::Result<i64> {
    env::var(name)
        .unwrap_or_else(|_| default_value.to_string())
        .parse::<i64>()
        .map_err(|err| anyhow::anyhow!("{name} must be an integer: {err}"))
}

fn parse_u64_env(name: &str, default_value: u64) -> anyhow::Result<u64> {
    env::var(name)
        .unwrap_or_else(|_| default_value.to_string())
        .parse::<u64>()
        .map_err(|err| anyhow::anyhow!("{name} must be an integer: {err}"))
}

fn parse_i32_env(name: &str, default_value: i32) -> anyhow::Result<i32> {
    env::var(name)
        .unwrap_or_else(|_| default_value.to_string())
        .parse::<i32>()
        .map_err(|err| anyhow::anyhow!("{name} must be an integer: {err}"))
}

fn parse_usize_env(name: &str, default_value: usize) -> anyhow::Result<usize> {
    env::var(name)
        .unwrap_or_else(|_| default_value.to_string())
        .parse::<usize>()
        .map_err(|err| anyhow::anyhow!("{name} must be an integer: {err}"))
}

fn parse_f64_env(name: &str, default_value: f64) -> anyhow::Result<f64> {
    env::var(name)
        .unwrap_or_else(|_| default_value.to_string())
        .parse::<f64>()
        .map_err(|err| anyhow::anyhow!("{name} must be a number: {err}"))
}

fn parse_bool_env(name: &str, default_value: bool) -> bool {
    env::var(name)
        .map(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no"
            )
        })
        .unwrap_or(default_value)
}

fn duration_from_secs_f64(seconds: f64) -> StdDuration {
    StdDuration::from_millis((seconds.max(0.001) * 1000.0).ceil() as u64)
}

fn is_production_environment() -> bool {
    ["APP_ENV", "ENVIRONMENT"].iter().any(|name| {
        matches!(
            env::var(name)
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "prod" | "production"
        )
    })
}

fn is_unsafe_admin_auth_value(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "admin"
            | "administrator"
            | "changeme"
            | "change-me"
            | "change_me"
            | "default"
            | "password"
            | "please-change-me"
            | "replace-me"
            | "secret"
            | "test"
            | "test-secret"
    ) || [
        "change-me",
        "change_me",
        "changeme",
        "change-this",
        "change_this",
        "changethis",
        "replace-me",
        "replace_me",
        "replace-this",
        "replace_this",
    ]
    .iter()
    .any(|placeholder| normalized.contains(placeholder))
}

fn validate_admin_auth_config(
    username: &str,
    password: &str,
    secret: &str,
    ttl_hours: i64,
) -> anyhow::Result<()> {
    if ttl_hours <= 0 {
        anyhow::bail!("ADMIN_AUTH_TTL_HOURS must be greater than zero");
    }
    if !is_production_environment() {
        return Ok(());
    }

    let mut unsafe_fields = Vec::new();
    if is_unsafe_admin_auth_value(username) {
        unsafe_fields.push("ADMIN_USERNAME");
    }
    if is_unsafe_admin_auth_value(password) {
        unsafe_fields.push("ADMIN_PASSWORD");
    }
    if is_unsafe_admin_auth_value(secret) {
        unsafe_fields.push("ADMIN_AUTH_SECRET");
    }
    if !unsafe_fields.is_empty() {
        anyhow::bail!(
            "Unsafe admin auth configuration for production: {} must not use default, weak, or change-me values",
            unsafe_fields.join(", ")
        );
    }
    if constant_time_eq(secret, password) {
        anyhow::bail!(
            "Unsafe admin auth configuration for production: ADMIN_AUTH_SECRET must not equal ADMIN_PASSWORD"
        );
    }
    if password.len() < 12 {
        anyhow::bail!(
            "Unsafe admin auth configuration for production: ADMIN_PASSWORD must be at least 12 characters"
        );
    }
    if secret.len() < 32 {
        anyhow::bail!(
            "Unsafe admin auth configuration for production: ADMIN_AUTH_SECRET must be at least 32 characters"
        );
    }
    Ok(())
}

fn normalize_cors_origin(origin: &str) -> anyhow::Result<String> {
    let origin = origin.trim().trim_end_matches('/');
    if origin == "*" {
        anyhow::bail!("CORS_ALLOWED_ORIGINS must list explicit origins, not '*'.");
    }
    let host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .ok_or_else(|| anyhow::anyhow!("CORS origins must start with http:// or https://"))?;
    if host.is_empty() || host.contains('/') || host.contains('?') || host.contains('#') {
        anyhow::bail!("CORS origins must not include paths, queries, fragments, or wildcards");
    }
    Ok(origin.to_string())
}

fn cors_allowed_origins() -> anyhow::Result<Vec<HeaderValue>> {
    let raw = env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost,http://127.0.0.1".into());
    raw.split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            let origin = normalize_cors_origin(origin)?;
            HeaderValue::from_str(&origin)
                .map_err(|err| anyhow::anyhow!("invalid CORS origin '{}': {}", origin, err))
        })
        .collect()
}

fn cors_layer(config: &Config) -> anyhow::Result<CorsLayer> {
    Ok(CorsLayer::new()
        .allow_origin(AllowOrigin::list(config.cors_allowed_origins.clone()))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::RANGE,
        ]))
}

impl AntdRestClient {
    fn new(base_url: &str, timeout_seconds: f64) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(StdDuration::from_secs(5))
                .timeout(duration_from_secs_f64(timeout_seconds))
                .build()?,
        })
    }

    async fn health(&self) -> anyhow::Result<AntdHealthResponse> {
        self.request_json(reqwest::Method::GET, "/health", Option::<Value>::None)
            .await
    }

    async fn wallet_address(&self) -> anyhow::Result<Value> {
        self.request_json(
            reqwest::Method::GET,
            "/v1/wallet/address",
            Option::<Value>::None,
        )
        .await
    }

    async fn wallet_balance(&self) -> anyhow::Result<Value> {
        self.request_json(
            reqwest::Method::GET,
            "/v1/wallet/balance",
            Option::<Value>::None,
        )
        .await
    }

    async fn wallet_approve(&self) -> anyhow::Result<Value> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/wallet/approve",
            Option::<Value>::None,
        )
        .await
    }

    async fn data_get_public(&self, address: &str) -> anyhow::Result<Vec<u8>> {
        let payload: AntdPublicDataResponse = self
            .request_json(
                reqwest::Method::GET,
                &format!("/v1/data/public/{}", address.trim()),
                Option::<Value>::None,
            )
            .await?;
        BASE64
            .decode(payload.data)
            .map_err(|err| anyhow::anyhow!("antd returned invalid base64 public data: {err}"))
    }

    async fn data_cost(&self, data: &[u8]) -> anyhow::Result<AntdDataCostResponse> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/data/cost",
            Some(json!({ "data": BASE64.encode(data) })),
        )
        .await
    }

    async fn data_cost_for_size(&self, byte_size: usize) -> anyhow::Result<AntdDataCostResponse> {
        let quote_size = byte_size.max(MIN_ANTD_SELF_ENCRYPTION_BYTES);
        let mut data = vec![0_u8; quote_size];
        rand::thread_rng().fill_bytes(&mut data);
        let mut last_error = None;
        for attempt in 1..=3 {
            match self.data_cost(&data).await {
                Ok(estimate) => return Ok(estimate),
                Err(err) => {
                    last_error = Some(err);
                    if attempt < 3 {
                        sleep(StdDuration::from_millis(100 * attempt as u64)).await;
                    }
                }
            }
        }
        Err(last_error
            .map(|err| {
                anyhow::anyhow!("Autonomi cost estimate failed for {quote_size} quote bytes: {err}")
            })
            .unwrap_or_else(|| {
                anyhow::anyhow!("Autonomi cost estimate failed for {quote_size} quote bytes")
            }))
    }

    async fn data_put_public(
        &self,
        data: &[u8],
        payment_mode: &str,
    ) -> anyhow::Result<AntdDataPutResponse> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/data/public",
            Some(json!({
                "data": BASE64.encode(data),
                "payment_mode": payment_mode,
            })),
        )
        .await
    }

    async fn file_put_public(
        &self,
        path: &FsPath,
        payment_mode: &str,
        verify: bool,
    ) -> anyhow::Result<AntdFilePutResponse> {
        let (_, sha256) = sha256_file_async(path).await?;
        let file = tokio_fs::File::open(path).await?;
        let stream = ReaderStream::new(file);
        let url = format!(
            "{}/v1/file/public?payment_mode={payment_mode}&verify={}",
            self.base_url, verify
        );
        let response = self
            .client
            .post(url)
            .header("content-type", "application/octet-stream")
            .header("x-content-sha256", sha256)
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("POST /v1/file/public failed: {} {}", status, text);
        }
        serde_json::from_str(&text)
            .map_err(|err| anyhow::anyhow!("POST /v1/file/public returned invalid JSON: {}", err))
    }

    async fn request_json<T>(
        &self,
        method: reqwest::Method,
        path: &str,
        json_body: Option<Value>,
    ) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let url = format!("{}{}", self.base_url, path);
        let mut request = self.client.request(method.clone(), url);
        if let Some(body) = json_body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            anyhow::bail!("{} {} failed: {} {}", method, path, status, text);
        }
        serde_json::from_str(&text)
            .map_err(|err| anyhow::anyhow!("{} {} returned invalid JSON: {}", method, path, err))
    }
}

fn is_missing_file_upload_endpoint(err: &anyhow::Error) -> bool {
    let message = err.to_string();
    if message.contains(" 404 ") || message.contains(" 405 ") || message.contains(" 501 ") {
        return true;
    }

    // Some antd-compatible servers close the connection as soon as they reject
    // the unsupported streaming route, while reqwest is still sending the body.
    // Treat that as "endpoint unavailable" so small media can use the legacy
    // JSON upload path instead of failing mid-stream.
    let message = message.to_ascii_lowercase();
    message.contains("/v1/file/public")
        && (message.contains("error sending request")
            || message.contains("connection reset")
            || message.contains("broken pipe")
            || message.contains("connection closed"))
}

async fn ensure_autonomi_ready(config: &Config, antd: &AntdRestClient) -> anyhow::Result<()> {
    let status = antd.health().await?;
    if !status.status.eq_ignore_ascii_case("ok") {
        anyhow::bail!("antd health check returned not ok");
    }
    let wallet = antd.wallet_address().await?;
    let balance = antd.wallet_balance().await?;
    info!(?wallet, ?balance, "Autonomi wallet ready");
    if config.antd_approve_on_startup {
        let approved = antd.wallet_approve().await?;
        info!(?approved, "Autonomi wallet spend approval checked");
    }
    if config.antd_require_cost_ready {
        antd.data_cost_for_size(MIN_ANTD_SELF_ENCRYPTION_BYTES)
            .await?;
        info!("Autonomi write cost probe succeeded");
    }
    Ok(())
}

async fn ensure_schema(pool: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
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
    let postgres = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => PostgresHealth {
            ok: true,
            error: None,
        },
        Err(err) => PostgresHealth {
            ok: false,
            error: Some(err.to_string()),
        },
    };
    let write_ready = if state.config.antd_require_cost_ready {
        state
            .antd
            .data_cost_for_size(MIN_ANTD_SELF_ENCRYPTION_BYTES)
            .await
            .is_ok()
    } else {
        autonomi.ok
    };
    let ok = autonomi.ok && postgres.ok && write_ready;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(HealthResponse {
            ok,
            autonomi,
            postgres,
            write_ready,
            payment_mode: state.config.antd_payment_mode.clone(),
            final_quote_approval_ttl_seconds: state.config.final_quote_approval_ttl_seconds,
            implementation: "rust_admin",
            role: "primary_admin",
        }),
    )
        .into_response()
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<AuthTokenOut>, ApiError> {
    if !constant_time_eq(&request.username, &state.config.admin_username)
        || !constant_time_eq(&request.password, &state.config.admin_password)
    {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "Invalid username or password",
        ));
    }

    let expires_at = Utc::now() + Duration::hours(state.config.admin_auth_ttl_hours);
    let token = encode(
        &Header::new(Algorithm::HS256),
        &Claims {
            sub: state.config.admin_username.clone(),
            exp: expires_at.timestamp() as usize,
        },
        &EncodingKey::from_secret(state.config.admin_auth_secret.as_bytes()),
    )
    .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;

    Ok(Json(AuthTokenOut {
        access_token: token,
        token_type: "bearer",
        expires_at: expires_at.to_rfc3339(),
        username: state.config.admin_username.clone(),
    }))
}

async fn auth_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminMeOut>, ApiError> {
    let username = require_admin(&state, &headers)?;
    Ok(Json(AdminMeOut { username }))
}

async fn get_catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers)?;
    let (catalog, catalog_address) = load_catalog(&state).await?;
    Ok(Json(json!({
        "catalog_address": catalog_address,
        "catalog": catalog,
    })))
}

async fn quote_video_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UploadQuoteRequest>,
) -> Result<Json<UploadQuoteOut>, ApiError> {
    require_admin(&state, &headers)?;
    build_upload_quote(&state, request).await.map(Json)
}

async fn list_videos(State(state): State<AppState>) -> Result<Json<Vec<VideoOut>>, ApiError> {
    let (catalog, catalog_address) = load_catalog(&state).await?;
    let videos = catalog
        .get("videos")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .iter()
        .filter(|entry| {
            entry
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or(STATUS_READY)
                == STATUS_READY
        })
        .map(|entry| catalog_entry_to_video_out(entry, catalog_address.as_deref()))
        .collect();
    Ok(Json(videos))
}

async fn admin_list_videos(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<VideoOut>>, ApiError> {
    require_admin(&state, &headers)?;
    let rows = sqlx::query(
        r#"
        SELECT id, title, original_filename, description, status, created_at,
               manifest_address, catalog_address, error_message, final_quote,
               final_quote_created_at, approval_expires_at,
               is_public, show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size,
               publish_when_ready
        FROM videos
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let mut videos = Vec::with_capacity(rows.len());
    for row in rows {
        videos.push(db_video_to_out(&state, &row, false).await?);
    }
    Ok(Json(videos))
}

async fn get_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
) -> Result<Json<VideoOut>, ApiError> {
    let (catalog, _) = load_catalog(&state).await?;
    let entry = catalog
        .get("videos")
        .and_then(Value::as_array)
        .and_then(|videos| {
            videos
                .iter()
                .find(|entry| entry.get("id").and_then(Value::as_str) == Some(video_id.as_str()))
        })
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let manifest_address = entry
        .get("manifest_address")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let manifest = load_json_from_autonomi(&state, manifest_address).await?;
    let mut video = manifest_to_video_out(&state, &manifest, Some(manifest_address), true);
    apply_catalog_visibility(&mut video, entry, &manifest, manifest_address);
    Ok(Json(video))
}

async fn admin_get_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn video_status(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let video_uuid = Uuid::parse_str(&video_id).ok();
    let row = sqlx::query(
        r#"
        SELECT status, manifest_address, catalog_address, error_message,
               show_manifest_address
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    if let Some(row) = row {
        let show_manifest_address = row
            .try_get::<bool, _>("show_manifest_address")
            .unwrap_or(false);
        let manifest_address = if show_manifest_address {
            row.try_get::<Option<String>, _>("manifest_address")
                .ok()
                .flatten()
        } else {
            None
        };
        return Ok(Json(json!({
            "video_id": video_id,
            "status": row.try_get::<String, _>("status").unwrap_or_default(),
            "manifest_address": manifest_address,
            "catalog_address": null,
            "error_message": row.try_get::<Option<String>, _>("error_message").ok().flatten(),
        })));
    }

    let loaded = load_video_manifest_by_id(&state, &video_id).await?;
    let (manifest, manifest_address) =
        loaded.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let show_manifest_address = manifest
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(Json(json!({
        "video_id": video_id,
        "status": STATUS_READY,
        "manifest_address": if show_manifest_address { Some(manifest_address) } else { None },
        "catalog_address": null,
    })))
}

async fn update_video_visibility(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<VideoVisibilityUpdate>,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;

    let row = sqlx::query(
        r#"
        UPDATE videos
        SET show_original_filename=$1,
            show_manifest_address=$2,
            updated_at=NOW()
        WHERE id=$3
        RETURNING status, is_public
        "#,
    )
    .bind(false)
    .bind(request.show_manifest_address)
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    let row = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let status: String = row.try_get("status").unwrap_or_default();
    let is_public: bool = row.try_get("is_public").unwrap_or(false);
    if status == STATUS_READY && is_public {
        let epoch = refresh_local_catalog_from_db(&state, "visibility").await?;
        schedule_catalog_publish(&state, epoch, format!("visibility:{video_id}")).await?;
    }

    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn upload_video(
    State(state): State<AppState>,
    headers: HeaderMap,
    multipart: Multipart,
) -> Result<Json<VideoOut>, ApiError> {
    let username = require_admin(&state, &headers)?;
    let accepted = accept_upload(&state, &headers, multipart, &username).await?;
    if let Err(err) = schedule_processing_job(&state, &accepted.video_id).await {
        let _ = set_status(&state, &accepted.video_id, STATUS_ERROR, Some(&err.detail)).await;
        if let Ok(Some(job_dir)) = fetch_job_dir(&state, &accepted.video_id).await {
            let _ = fs::remove_dir_all(job_dir);
        }
        return Err(err);
    }
    Ok(Json(accepted.video))
}

async fn approve_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    cleanup_expired_approvals(&state).await.map_err(db_error)?;
    let video_uuid = parse_video_uuid(&video_id)?;

    let mut expired_job_dir = None;
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    let row = sqlx::query(
        r#"
        SELECT status, approval_expires_at, job_dir
        FROM videos
        WHERE id=$1
        FOR UPDATE
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let status: String = row.try_get("status").unwrap_or_default();
    if status != STATUS_AWAITING_APPROVAL {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!("Video is {status}, not awaiting approval"),
        ));
    }

    let job_dir: Option<String> = row.try_get("job_dir").ok().flatten();
    let approval_expires_at: Option<DateTime<Utc>> =
        row.try_get("approval_expires_at").ok().flatten();
    if approval_expires_at.is_some_and(|expires_at| expires_at <= Utc::now()) {
        expired_job_dir = job_dir.clone();
        sqlx::query(
            r#"
            UPDATE videos
            SET status='expired',
                error_message='Final quote approval window expired; local files were deleted.',
                updated_at=NOW()
            WHERE id=$1
            "#,
        )
        .bind(video_uuid)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    } else if job_dir
        .as_deref()
        .map(|path| !FsPath::new(path).exists())
        .unwrap_or(true)
    {
        return Err(ApiError::new(
            StatusCode::GONE,
            "Transcoded files are no longer available",
        ));
    } else {
        sqlx::query(
            r#"
            UPDATE videos
            SET status='uploading', error_message=NULL, updated_at=NOW()
            WHERE id=$1
            "#,
        )
        .bind(video_uuid)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    }
    tx.commit().await.map_err(db_error)?;

    if let Some(path) = expired_job_dir {
        let _ = fs::remove_dir_all(path);
        return Err(ApiError::new(
            StatusCode::GONE,
            "Final quote approval window has expired",
        ));
    }

    if let Err(err) = schedule_upload_job(&state, &video_id).await {
        let _ = set_status(&state, &video_id, STATUS_ERROR, Some(&err.detail)).await;
        return Err(err);
    }
    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn update_video_publication(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<VideoPublicationUpdate>,
) -> Result<Json<VideoOut>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;
    let row = sqlx::query("SELECT status, manifest_address FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    if request.is_public {
        let status: String = row.try_get("status").unwrap_or_default();
        if status != STATUS_READY {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "Only ready videos can be published",
            ));
        }
        let manifest_address = if let Some(address) = row
            .try_get::<Option<String>, _>("manifest_address")
            .ok()
            .flatten()
        {
            address
        } else {
            ensure_video_manifest_address(&state, &video_id).await?
        };
        set_publication(&state, &video_id, true, Some(&manifest_address), None).await?;
    } else {
        set_publication(&state, &video_id, false, None, None).await?;
    }

    let reason = if request.is_public {
        "publish"
    } else {
        "unpublish"
    };
    let epoch = refresh_local_catalog_from_db(&state, reason).await?;
    schedule_catalog_publish(&state, epoch, format!("{reason}:{video_id}")).await?;

    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn delete_video(
    State(state): State<AppState>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_admin(&state, &headers)?;
    let video_uuid = parse_video_uuid(&video_id)?;
    let job_dir_row = sqlx::query("SELECT job_dir FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?;
    let result = sqlx::query("DELETE FROM videos WHERE id=$1")
        .bind(video_uuid)
        .execute(&state.pool)
        .await
        .map_err(db_error)?;

    if result.rows_affected() == 0 {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "Video not found"));
    }

    let epoch = refresh_local_catalog_from_db(&state, "delete").await?;
    schedule_catalog_publish(&state, epoch, format!("delete:{video_id}")).await?;

    if let Some(row) = job_dir_row {
        if let Ok(Some(job_dir)) = row.try_get::<Option<String>, _>("job_dir") {
            let _ = fs::remove_dir_all(job_dir);
        }
    }
    let _ = fs::remove_dir_all(state.config.upload_temp_dir.join(&video_id));

    Ok(Json(json!({
        "deleted": video_id,
        "catalog_address": read_catalog_address(&state.config),
    })))
}

async fn accept_upload(
    state: &AppState,
    headers: &HeaderMap,
    mut multipart: Multipart,
    username: &str,
) -> Result<AcceptedUpload, ApiError> {
    let multipart_overhead_allowance = 2 * 1024 * 1024_u64;
    if let Some(content_length) = content_length(headers) {
        if content_length > state.config.upload_max_file_bytes + multipart_overhead_allowance {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Upload exceeds max file size ({})",
                    format_bytes(state.config.upload_max_file_bytes)
                ),
            ));
        }
    }

    let _permit = state
        .upload_save_semaphore
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many uploads are in progress; try again shortly",
            )
        })?;

    let video_uuid = Uuid::new_v4();
    let video_id = video_uuid.to_string();
    let job_dir = state.config.upload_temp_dir.join(&video_id);
    fs::create_dir_all(&job_dir).map_err(|err| {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!("Could not create upload directory: {err}"),
        )
    })?;
    let guard = JobDirGuard::new(job_dir.clone());

    if let Some(content_length) = content_length(headers) {
        ensure_upload_disk_space(state, content_length)?;
    } else {
        ensure_upload_disk_space(state, 0)?;
    }

    let mut title: Option<String> = None;
    let mut description = String::new();
    let mut resolutions = String::from("720p");
    let mut show_manifest_address = false;
    let mut upload_original = false;
    let mut publish_when_ready = false;
    let mut original_filename: Option<String> = None;
    let mut source_path: Option<PathBuf> = None;
    let mut upload_metadata: Option<UploadMediaMetadata> = None;

    while let Some(mut field) = multipart.next_field().await.map_err(|err| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("Invalid multipart upload: {err}"),
        )
    })? {
        let Some(name) = field.name().map(str::to_string) else {
            continue;
        };

        if name == "file" {
            if source_path.is_some() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "Only one upload file is supported",
                ));
            }
            let safe_filename = sanitize_upload_filename(field.file_name());
            let src_path = job_dir.join(format!("original_{safe_filename}"));
            let tmp_src_path = src_path.with_file_name(format!(
                "{}.uploading",
                src_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("upload")
            ));
            let mut output = tokio_fs::File::create(&tmp_src_path).await.map_err(|err| {
                ApiError::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    format!("Could not store upload safely: {err}"),
                )
            })?;
            let mut bytes_written = 0_u64;
            while let Some(chunk) = field.chunk().await.map_err(|err| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("Could not read upload: {err}"),
                )
            })? {
                let next_size = bytes_written + chunk.len() as u64;
                if next_size > state.config.upload_max_file_bytes {
                    let _ = tokio_fs::remove_file(&tmp_src_path).await;
                    return Err(ApiError::new(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        format!(
                            "Upload exceeds max file size ({})",
                            format_bytes(state.config.upload_max_file_bytes)
                        ),
                    ));
                }
                ensure_upload_disk_space(state, chunk.len() as u64)?;
                output.write_all(&chunk).await.map_err(|err| {
                    ApiError::new(
                        StatusCode::INSUFFICIENT_STORAGE,
                        format!("Could not store upload safely: {err}"),
                    )
                })?;
                bytes_written = next_size;
            }
            output.flush().await.map_err(|err| {
                ApiError::new(
                    StatusCode::INSUFFICIENT_STORAGE,
                    format!("Could not store upload safely: {err}"),
                )
            })?;
            drop(output);
            if bytes_written == 0 {
                let _ = tokio_fs::remove_file(&tmp_src_path).await;
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "Uploaded file is empty",
                ));
            }

            let metadata = probe_upload_media(state, &tmp_src_path).await?;
            tokio_fs::rename(&tmp_src_path, &src_path)
                .await
                .map_err(|err| {
                    ApiError::new(
                        StatusCode::INSUFFICIENT_STORAGE,
                        format!("Could not store upload safely: {err}"),
                    )
                })?;
            info!(
                "Accepted upload {} filename={} bytes={} duration={:.2}s dimensions={}x{}",
                video_id,
                safe_filename,
                bytes_written,
                metadata.duration_seconds,
                metadata.dimensions.0,
                metadata.dimensions.1
            );
            original_filename = Some(safe_filename);
            source_path = Some(src_path);
            upload_metadata = Some(metadata);
        } else {
            let text = field.text().await.map_err(|err| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("Invalid form field {name}: {err}"),
                )
            })?;
            match name.as_str() {
                "title" => title = Some(text.trim().to_string()),
                "description" => description = text.trim().to_string(),
                "resolutions" => resolutions = text,
                "show_original_filename" => {}
                "show_manifest_address" => show_manifest_address = parse_form_bool(&text),
                "upload_original" => upload_original = parse_form_bool(&text),
                "publish_when_ready" => publish_when_ready = parse_form_bool(&text),
                _ => {}
            }
        }
    }

    let title = title
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "title is required"))?;
    let original_filename = original_filename
        .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "file is required"))?;
    let source_path =
        source_path.ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "file is required"))?;
    let selected = parse_resolutions(&resolutions);
    if selected.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            supported_resolutions_error(),
        ));
    }
    if let Some(metadata) = upload_metadata {
        enforce_upload_media_limits(
            state,
            metadata.duration_seconds,
            metadata.dimensions.0,
            metadata.dimensions.1,
        )?;
    }

    sqlx::query(
        r#"
        INSERT INTO videos (
            id, title, original_filename, description, status, job_dir,
            job_source_path, requested_resolutions,
            show_original_filename, show_manifest_address,
            upload_original, publish_when_ready, user_id
        )
        VALUES ($1, $2, $3, $4, 'pending', $5, $6, $7::jsonb, $8, $9, $10, $11, $12)
        "#,
    )
    .bind(video_uuid)
    .bind(&title)
    .bind(&original_filename)
    .bind(if description.is_empty() {
        None
    } else {
        Some(description.as_str())
    })
    .bind(job_dir.to_string_lossy().as_ref())
    .bind(source_path.to_string_lossy().as_ref())
    .bind(json!(selected))
    .bind(false)
    .bind(show_manifest_address)
    .bind(upload_original)
    .bind(publish_when_ready)
    .bind(username)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    let video = get_db_video(state, &video_id, false).await?;
    guard.disarm();
    Ok(AcceptedUpload { video_id, video })
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
}

fn parse_form_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_resolutions(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|resolution| resolution_preset(resolution).is_some())
        .map(str::to_string)
        .collect()
}

fn sanitize_upload_filename(filename: Option<&str>) -> String {
    let basename = filename
        .and_then(|name| FsPath::new(name).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("upload");
    let path = FsPath::new(basename);
    let raw_stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("upload");
    let raw_suffix = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let mut safe_stem: String = raw_stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(&['.', '_', '-'][..])
        .to_string();
    if safe_stem.is_empty() {
        safe_stem = "upload".to_string();
    }
    let mut safe_suffix: String = raw_suffix
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .take(15)
        .collect::<String>()
        .to_ascii_lowercase();
    if !safe_suffix.is_empty() {
        safe_suffix.insert(0, '.');
    }
    let max_stem_length = 128_usize.saturating_sub(safe_suffix.len()).max(1);
    if safe_stem.len() > max_stem_length {
        safe_stem.truncate(max_stem_length);
    }
    format!("{safe_stem}{safe_suffix}")
}

fn ensure_upload_disk_space(state: &AppState, additional_bytes: u64) -> Result<(), ApiError> {
    let free_bytes = fs2::available_space(&state.config.upload_temp_dir).map_err(|err| {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!("Could not inspect upload disk space: {err}"),
        )
    })?;
    let required_free = state
        .config
        .upload_min_free_bytes
        .saturating_add(additional_bytes);
    if free_bytes < required_free {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            format!(
                "Not enough upload disk space (free={}, required={})",
                format_bytes(free_bytes),
                format_bytes(required_free)
            ),
        ));
    }
    Ok(())
}

fn format_bytes(byte_count: u64) -> String {
    let mut value = byte_count as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 || unit == "TiB" {
            if unit == "B" {
                return format!("{byte_count} B");
            }
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{byte_count} B")
}

async fn sha256_file_async(path: &FsPath) -> anyhow::Result<(u64, String)> {
    let mut file = tokio_fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut byte_size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        byte_size += read as u64;
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    Ok((byte_size, hex_lower(&digest)))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

async fn run_command_output(
    mut command: Command,
    timeout_seconds: Option<f64>,
) -> Result<CommandOutput, ApiError> {
    let child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not start media tool: {err}"),
            )
        })?;

    let wait = child.wait_with_output();
    let output = if let Some(seconds) = timeout_seconds {
        tokio::time::timeout(duration_from_secs_f64(seconds), wait)
            .await
            .map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "Could not validate uploaded media before timeout",
                )
            })?
    } else {
        wait.await
    }
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Media tool failed to run: {err}"),
        )
    })?;

    Ok(CommandOutput {
        status_code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

async fn probe_upload_media(
    state: &AppState,
    src: &FsPath,
) -> Result<UploadMediaMetadata, ApiError> {
    let mut command = Command::new("ffprobe");
    command
        .arg("-v")
        .arg("error")
        .arg("-show_streams")
        .arg("-show_format")
        .arg("-of")
        .arg("json")
        .arg(src);
    let output =
        run_command_output(command, Some(state.config.upload_ffprobe_timeout_seconds)).await?;
    if output.status_code != Some(0) {
        let detail = stderr_tail(&output.stderr, 500);
        let message = if detail.is_empty() {
            "Uploaded file is not a readable video".to_string()
        } else {
            format!("Uploaded file is not a readable video: {detail}")
        };
        return Err(ApiError::new(StatusCode::BAD_REQUEST, message));
    }

    let data: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "Uploaded file probe returned invalid metadata",
        )
    })?;
    let stream = data
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| {
            streams
                .iter()
                .find(|stream| stream.get("codec_type").and_then(Value::as_str) == Some("video"))
        })
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "Uploaded file does not contain a video stream",
            )
        })?;

    let width = stream.get("width").and_then(Value::as_i64).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "Uploaded video stream has no usable dimensions",
        )
    })? as i32;
    let height = stream
        .get("height")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "Uploaded video stream has no usable dimensions",
            )
        })? as i32;
    if width <= 0 || height <= 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "Uploaded video stream has invalid dimensions",
        ));
    }
    let dimensions =
        if stream_rotation_degrees(stream) == 90 || stream_rotation_degrees(stream) == 270 {
            (height, width)
        } else {
            (width, height)
        };
    let duration_seconds = parse_probe_duration(&data, stream).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "Uploaded video has no usable duration",
        )
    })?;
    enforce_upload_media_limits(state, duration_seconds, dimensions.0, dimensions.1)?;
    Ok(UploadMediaMetadata {
        duration_seconds,
        dimensions,
    })
}

async fn probe_duration(src: &FsPath) -> Result<Option<f64>, ApiError> {
    let mut command = Command::new("ffprobe");
    command
        .arg("-v")
        .arg("quiet")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(src);
    let output = run_command_output(command, None).await?;
    if output.status_code != Some(0) {
        return Ok(None);
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    Ok(raw
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0))
}

async fn probe_video_dimensions(src: &FsPath) -> Result<Option<(i32, i32)>, ApiError> {
    let mut command = Command::new("ffprobe");
    command
        .arg("-v")
        .arg("quiet")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_streams")
        .arg("-of")
        .arg("json")
        .arg(src);
    let output = run_command_output(command, None).await?;
    if output.status_code != Some(0) {
        return Ok(None);
    }
    let Ok(data) = serde_json::from_slice::<Value>(&output.stdout) else {
        return Ok(None);
    };
    let Some(stream) = data
        .get("streams")
        .and_then(Value::as_array)
        .and_then(|streams| streams.first())
    else {
        return Ok(None);
    };
    let Some(width) = stream
        .get("width")
        .and_then(Value::as_i64)
        .map(|value| value as i32)
    else {
        return Ok(None);
    };
    let Some(height) = stream
        .get("height")
        .and_then(Value::as_i64)
        .map(|value| value as i32)
    else {
        return Ok(None);
    };
    if stream_rotation_degrees(stream) == 90 || stream_rotation_degrees(stream) == 270 {
        Ok(Some((height, width)))
    } else {
        Ok(Some((width, height)))
    }
}

async fn run_ffmpeg(
    state: &AppState,
    src: &FsPath,
    seg_dir: &FsPath,
    width: i32,
    height: i32,
    video_kbps: i32,
    audio_kbps: i32,
) -> Result<(), ApiError> {
    let segment_pattern = seg_dir.join("seg_%05d.ts");
    let segment_time = format!("{}", F64Format(state.config.hls_segment_duration));
    let mut command = Command::new("ffmpeg");
    command
        .arg("-hide_banner")
        .arg("-nostats")
        .arg("-loglevel")
        .arg("warning")
        .arg("-y")
        .arg("-filter_threads")
        .arg(state.config.ffmpeg_filter_threads.to_string())
        .arg("-i")
        .arg(src)
        .arg("-map")
        .arg("0:v:0")
        .arg("-map")
        .arg("0:a?")
        .arg("-sn")
        .arg("-c:v")
        .arg("libx264")
        .arg("-threads")
        .arg(state.config.ffmpeg_threads.to_string())
        .arg("-preset")
        .arg("veryfast")
        .arg("-profile:v")
        .arg("high")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-vf")
        .arg(format!(
            "scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2"
        ))
        .arg("-b:v")
        .arg(format!("{video_kbps}k"))
        .arg("-maxrate")
        .arg(format!("{}k", video_kbps * 3 / 2))
        .arg("-bufsize")
        .arg(format!("{}k", video_kbps * 2))
        .arg("-force_key_frames")
        .arg(format!("expr:gte(t,n_forced*{segment_time})"))
        .arg("-sc_threshold")
        .arg("0")
        .arg("-c:a")
        .arg("aac")
        .arg("-b:a")
        .arg(format!("{audio_kbps}k"))
        .arg("-ar")
        .arg("44100")
        .arg("-f")
        .arg("segment")
        .arg("-segment_time")
        .arg(segment_time)
        .arg("-segment_time_delta")
        .arg("0.05")
        .arg("-segment_format")
        .arg("mpegts")
        .arg("-reset_timestamps")
        .arg("1")
        .arg(segment_pattern);
    let output = run_command_output(command, None).await?;
    if output.status_code != Some(0) {
        let mut detail = stderr_tail(&output.stderr, 2000);
        if output.status_code == Some(137) {
            detail = format!(
                "FFmpeg was killed, which usually means the container ran out of memory while transcoding. FFMPEG_THREADS={}, FFMPEG_FILTER_THREADS={}. {detail}",
                state.config.ffmpeg_threads, state.config.ffmpeg_filter_threads
            );
        }
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "FFmpeg failed with exit code {:?}: {detail}",
                output.status_code
            ),
        ));
    }
    Ok(())
}

struct F64Format(f64);

impl std::fmt::Display for F64Format {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = if (self.0.fract()).abs() < f64::EPSILON {
            format!("{}", self.0 as i64)
        } else {
            let mut text = format!("{:.6}", self.0);
            while text.contains('.') && text.ends_with('0') {
                text.pop();
            }
            text
        };
        formatter.write_str(&text)
    }
}

fn stderr_tail(stderr: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(stderr);
    let start = text.len().saturating_sub(limit);
    text[start..].trim().to_string()
}

fn stream_rotation_degrees(stream: &Value) -> i32 {
    let rotation = stream
        .get("tags")
        .and_then(|tags| tags.get("rotate"))
        .and_then(value_to_i32)
        .or_else(|| {
            stream
                .get("side_data_list")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .find_map(|item| item.get("rotation").and_then(value_to_i32))
                })
        })
        .unwrap_or(0);
    rotation.rem_euclid(360)
}

fn value_to_i32(value: &Value) -> Option<i32> {
    value
        .as_i64()
        .map(|value| value as i32)
        .or_else(|| value.as_f64().map(|value| value as i32))
        .or_else(|| {
            value
                .as_str()?
                .parse::<f64>()
                .ok()
                .map(|value| value as i32)
        })
}

fn parse_probe_duration(data: &Value, stream: &Value) -> Option<f64> {
    [stream, data.get("format").unwrap_or(&Value::Null)]
        .into_iter()
        .filter_map(|source| {
            source
                .get("duration")
                .and_then(|value| {
                    value
                        .as_f64()
                        .or_else(|| value.as_str()?.parse::<f64>().ok())
                })
                .filter(|value| value.is_finite() && *value > 0.0)
        })
        .next()
}

fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("Login required"))?;
    let token = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ApiError::unauthorized("Login required"))?;

    let claims = decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.config.admin_auth_secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .map_err(|_| ApiError::unauthorized("Invalid or expired login"))?
    .claims;

    if claims.sub != state.config.admin_username {
        return Err(ApiError::unauthorized("Invalid or expired login"));
    }
    Ok(claims.sub)
}

fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

fn read_catalog_state_value(config: &Config) -> Option<Value> {
    let raw = match fs::read_to_string(&config.catalog_state_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            warn!(
                path = %config.catalog_state_path.display(),
                "Could not read catalog state file: {err}"
            );
            return None;
        }
    };

    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => Some(value),
        Err(err) => {
            let broken_path = catalog_state_broken_path(&config.catalog_state_path);
            match fs::rename(&config.catalog_state_path, &broken_path) {
                Ok(()) => warn!(
                    path = %config.catalog_state_path.display(),
                    broken_path = %broken_path.display(),
                    "Quarantined invalid catalog state file: {err}"
                ),
                Err(rename_err) => warn!(
                    path = %config.catalog_state_path.display(),
                    broken_path = %broken_path.display(),
                    "Invalid catalog state file could not be quarantined: {err}; rename failed: {rename_err}"
                ),
            }
            None
        }
    }
}

fn catalog_state_broken_path(path: &FsPath) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("catalog.json");
    path.with_file_name(format!("{file_name}.broken"))
}

fn catalog_address_from_state(value: &Value) -> Option<String> {
    value
        .get("catalog_address")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(ToOwned::to_owned)
}

fn read_catalog_address(config: &Config) -> Option<String> {
    read_catalog_state_value(config)
        .as_ref()
        .and_then(catalog_address_from_state)
        .or_else(|| config.catalog_bootstrap_address.clone())
}

fn read_catalog_snapshot(config: &Config) -> Option<(Value, Option<String>)> {
    let value = read_catalog_state_value(config)?;
    let mut catalog = value.get("catalog")?.clone();
    if !catalog.is_object() {
        return None;
    }
    if !catalog.get("videos").is_some_and(Value::is_array) {
        catalog["videos"] = json!([]);
    }
    Some((
        catalog,
        catalog_address_from_state(&value).or_else(|| config.catalog_bootstrap_address.clone()),
    ))
}

fn empty_catalog() -> Value {
    json!({
        "schema_version": 1,
        "content_type": CATALOG_CONTENT_TYPE,
        "updated_at": Utc::now().to_rfc3339(),
        "videos": [],
    })
}

async fn load_catalog(state: &AppState) -> Result<(Value, Option<String>), ApiError> {
    if let Some(snapshot) = read_catalog_snapshot(&state.config) {
        return Ok(snapshot);
    }

    let Some(address) = read_catalog_address(&state.config) else {
        return Ok((empty_catalog(), None));
    };

    match load_json_from_autonomi(state, &address).await {
        Ok(mut catalog) => {
            if !catalog.get("videos").is_some_and(Value::is_array) {
                catalog["videos"] = json!([]);
            }
            Ok((catalog, Some(address)))
        }
        Err(err) => {
            error!("Could not load Autonomi catalog {}: {:?}", address, err);
            Ok((empty_catalog(), Some(address)))
        }
    }
}

async fn load_json_from_autonomi(state: &AppState, address: &str) -> Result<Value, ApiError> {
    let data = state
        .antd
        .data_get_public(address)
        .await
        .map_err(|err| ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()))?;
    serde_json::from_slice(&data).map_err(|err| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("invalid JSON from Autonomi: {err}"),
        )
    })
}

async fn load_video_manifest_by_id(
    state: &AppState,
    video_id: &str,
) -> Result<Option<(Value, String)>, ApiError> {
    let (catalog, _) = load_catalog(state).await?;
    let Some(manifest_address) = catalog
        .get("videos")
        .and_then(Value::as_array)
        .and_then(|videos| {
            videos
                .iter()
                .find(|entry| entry.get("id").and_then(Value::as_str) == Some(video_id))
        })
        .and_then(|entry| entry.get("manifest_address").and_then(Value::as_str))
    else {
        return Ok(None);
    };

    let manifest = load_json_from_autonomi(state, manifest_address).await?;
    Ok(Some((manifest, manifest_address.to_string())))
}

async fn get_db_video(
    state: &AppState,
    video_id: &str,
    include_segments: bool,
) -> Result<VideoOut, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let row = sqlx::query(
        r#"
        SELECT id, title, original_filename, description, status, created_at,
               manifest_address, catalog_address, error_message, final_quote,
               final_quote_created_at, approval_expires_at,
               is_public, show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size,
               publish_when_ready
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    db_video_to_out(state, &row, include_segments).await
}

async fn db_video_to_out(
    state: &AppState,
    row: &sqlx::postgres::PgRow,
    include_segments: bool,
) -> Result<VideoOut, ApiError> {
    let video_id: Uuid = row.try_get("id").map_err(db_error)?;
    let variant_rows = sqlx::query(
        r#"
        SELECT id, resolution, width, height, total_duration, segment_count
        FROM video_variants WHERE video_id=$1 ORDER BY height DESC
        "#,
    )
    .bind(video_id)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let mut variants = Vec::with_capacity(variant_rows.len());
    for variant in variant_rows {
        let variant_id: Uuid = variant.try_get("id").map_err(db_error)?;
        let mut segments = Vec::new();
        if include_segments {
            let segment_rows = sqlx::query(
                r#"
                SELECT segment_index, autonomi_address, duration
                FROM video_segments WHERE variant_id=$1 ORDER BY segment_index
                "#,
            )
            .bind(variant_id)
            .fetch_all(&state.pool)
            .await
            .map_err(db_error)?;
            segments = segment_rows
                .into_iter()
                .map(|segment| SegmentOut {
                    segment_index: segment.try_get("segment_index").unwrap_or_default(),
                    autonomi_address: segment.try_get("autonomi_address").ok().flatten(),
                    duration: segment.try_get("duration").unwrap_or_default(),
                })
                .collect();
        }
        variants.push(VariantOut {
            id: variant_id.to_string(),
            resolution: variant.try_get("resolution").unwrap_or_default(),
            width: variant.try_get("width").unwrap_or_default(),
            height: variant.try_get("height").unwrap_or_default(),
            total_duration: variant.try_get("total_duration").ok().flatten(),
            segment_count: variant.try_get("segment_count").ok().flatten(),
            segments,
        });
    }

    let created_at: DateTime<Utc> = row.try_get("created_at").map_err(db_error)?;
    let final_quote_created_at: Option<DateTime<Utc>> =
        row.try_get("final_quote_created_at").ok().flatten();
    let approval_expires_at: Option<DateTime<Utc>> =
        row.try_get("approval_expires_at").ok().flatten();
    let catalog_address = row
        .try_get::<Option<String>, _>("catalog_address")
        .ok()
        .flatten()
        .or_else(|| read_catalog_address(&state.config));

    Ok(VideoOut {
        id: video_id.to_string(),
        title: row.try_get("title").unwrap_or_default(),
        original_filename: row.try_get("original_filename").ok().flatten(),
        description: row.try_get("description").ok().flatten(),
        status: row.try_get("status").unwrap_or_default(),
        created_at: created_at.to_rfc3339(),
        manifest_address: row.try_get("manifest_address").ok().flatten(),
        catalog_address,
        is_public: row.try_get("is_public").unwrap_or(false),
        show_original_filename: row.try_get("show_original_filename").unwrap_or(false),
        show_manifest_address: row.try_get("show_manifest_address").unwrap_or(false),
        upload_original: row.try_get("upload_original").unwrap_or(false),
        original_file_address: row.try_get("original_file_address").ok().flatten(),
        original_file_byte_size: row.try_get("original_file_byte_size").ok().flatten(),
        publish_when_ready: row.try_get("publish_when_ready").unwrap_or(false),
        error_message: row.try_get("error_message").ok().flatten(),
        final_quote: row.try_get("final_quote").ok().flatten(),
        final_quote_created_at: final_quote_created_at.map(|value| value.to_rfc3339()),
        approval_expires_at: approval_expires_at.map(|value| value.to_rfc3339()),
        variants,
    })
}

fn catalog_entry_to_video_out(entry: &Value, catalog_address: Option<&str>) -> VideoOut {
    let show_manifest_address = entry
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    VideoOut {
        id: string_field(entry, "id"),
        title: string_field(entry, "title"),
        original_filename: None,
        description: opt_string_field(entry, "description"),
        status: opt_string_field(entry, "status").unwrap_or_else(|| STATUS_READY.into()),
        created_at: string_field(entry, "created_at"),
        manifest_address: if show_manifest_address {
            opt_string_field(entry, "manifest_address")
        } else {
            None
        },
        catalog_address: catalog_address.map(str::to_string),
        is_public: true,
        show_original_filename: false,
        show_manifest_address,
        upload_original: false,
        original_file_address: None,
        original_file_byte_size: None,
        publish_when_ready: false,
        error_message: None,
        final_quote: None,
        final_quote_created_at: None,
        approval_expires_at: None,
        variants: entry
            .get("variants")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|variant| VariantOut {
                id: format!(
                    "{}:{}",
                    string_field(entry, "id"),
                    string_field(variant, "resolution")
                ),
                resolution: string_field(variant, "resolution"),
                width: int_field(variant, "width"),
                height: int_field(variant, "height"),
                total_duration: variant.get("total_duration").and_then(Value::as_f64),
                segment_count: variant
                    .get("segment_count")
                    .and_then(Value::as_i64)
                    .map(|value| value as i32),
                segments: vec![],
            })
            .collect(),
    }
}

fn manifest_to_video_out(
    state: &AppState,
    manifest: &Value,
    manifest_address: Option<&str>,
    public: bool,
) -> VideoOut {
    let show_original_filename = manifest
        .get("show_original_filename")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let show_manifest_address = manifest
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let original_file = manifest
        .get("original_file")
        .filter(|value| value.is_object());
    let video_id = string_field(manifest, "id");
    VideoOut {
        id: video_id.clone(),
        title: string_field(manifest, "title"),
        original_filename: if !public {
            opt_string_field(manifest, "original_filename")
        } else {
            None
        },
        description: opt_string_field(manifest, "description"),
        status: opt_string_field(manifest, "status").unwrap_or_else(|| STATUS_READY.into()),
        created_at: string_field(manifest, "created_at"),
        manifest_address: if !public || show_manifest_address {
            manifest_address
                .map(str::to_string)
                .or_else(|| opt_string_field(manifest, "manifest_address"))
        } else {
            None
        },
        catalog_address: if public {
            None
        } else {
            read_catalog_address(&state.config)
        },
        is_public: public,
        show_original_filename: if public {
            false
        } else {
            show_original_filename
        },
        show_manifest_address,
        upload_original: original_file.is_some(),
        original_file_address: if public {
            None
        } else {
            original_file
                .and_then(|value| value.get("autonomi_address"))
                .and_then(Value::as_str)
                .map(str::to_string)
        },
        original_file_byte_size: if public {
            None
        } else {
            original_file
                .and_then(|value| value.get("byte_size"))
                .and_then(Value::as_i64)
        },
        publish_when_ready: false,
        error_message: None,
        final_quote: None,
        final_quote_created_at: None,
        approval_expires_at: None,
        variants: manifest
            .get("variants")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|variant| VariantOut {
                id: format!("{video_id}:{}", string_field(variant, "resolution")),
                resolution: string_field(variant, "resolution"),
                width: int_field(variant, "width"),
                height: int_field(variant, "height"),
                total_duration: variant.get("total_duration").and_then(Value::as_f64),
                segment_count: variant
                    .get("segment_count")
                    .and_then(Value::as_i64)
                    .map(|value| value as i32),
                segments: if public {
                    vec![]
                } else {
                    variant
                        .get("segments")
                        .and_then(Value::as_array)
                        .unwrap_or(&Vec::new())
                        .iter()
                        .map(|segment| SegmentOut {
                            segment_index: int_field(segment, "segment_index"),
                            autonomi_address: opt_string_field(segment, "autonomi_address"),
                            duration: segment
                                .get("duration")
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0),
                        })
                        .collect()
                },
            })
            .collect(),
    }
}

fn apply_catalog_visibility(
    video: &mut VideoOut,
    entry: &Value,
    _manifest: &Value,
    manifest_address: &str,
) {
    let show_manifest_address = entry
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    video.show_original_filename = false;
    video.show_manifest_address = show_manifest_address;
    video.original_filename = None;
    video.manifest_address = if show_manifest_address {
        Some(manifest_address.to_string())
    } else {
        None
    };
    video.original_file_address = None;
    video.original_file_byte_size = None;
}

async fn schedule_processing_job(state: &AppState, video_id: &str) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    enqueue_video_job(state, JobKind::ProcessVideo, video_uuid).await
}

async fn schedule_upload_job(state: &AppState, video_id: &str) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    enqueue_video_job(state, JobKind::UploadVideo, video_uuid).await
}

fn start_job_workers(state: &AppState) {
    for worker_index in 0..state.config.admin_job_workers {
        let worker_id = format!("admin-{}-{worker_index}", Uuid::new_v4());
        tokio::spawn(job_worker_loop(state.clone(), worker_id));
    }
    info!(
        "Started {} durable admin job worker(s)",
        state.config.admin_job_workers
    );
}

async fn job_worker_loop(state: AppState, worker_id: String) {
    loop {
        match acquire_next_job(&state, &worker_id).await {
            Ok(Some(job)) => {
                let kind = job.kind;
                let job_id = job.id;
                let result = run_leased_job(&state, &job).await;
                match result {
                    Ok(()) => {
                        if let Err(err) = mark_job_succeeded(&state, job_id).await {
                            warn!(
                                "Worker {} could not mark {:?} job {} succeeded: {}",
                                worker_id, kind, job_id, err.detail
                            );
                        }
                    }
                    Err(err) => {
                        let detail = err.detail;
                        warn!(
                            "Worker {} {:?} job {} failed on attempt {}/{}: {}",
                            worker_id, kind, job_id, job.attempts, job.max_attempts, detail
                        );
                        if let Err(mark_err) = mark_job_failed(&state, &job, &detail).await {
                            warn!(
                                "Worker {} could not persist {:?} job {} failure: {}",
                                worker_id, kind, job_id, mark_err.detail
                            );
                        }
                    }
                }
            }
            Ok(None) => {
                sleep(StdDuration::from_secs(
                    state.config.admin_job_poll_interval_seconds,
                ))
                .await;
            }
            Err(err) => {
                warn!("Worker {} could not lease a job: {}", worker_id, err.detail);
                sleep(StdDuration::from_secs(
                    state.config.admin_job_poll_interval_seconds,
                ))
                .await;
            }
        }
    }
}

async fn enqueue_video_job(
    state: &AppState,
    kind: JobKind,
    video_id: Uuid,
) -> Result<(), ApiError> {
    let result = sqlx::query(
        r#"
        INSERT INTO video_jobs (job_kind, video_id, status, max_attempts, run_after)
        SELECT $1, $2, $3, $4, NOW()
        WHERE NOT EXISTS (
            SELECT 1 FROM video_jobs
            WHERE job_kind=$1
              AND video_id=$2
              AND status IN ($3, $5)
        )
        "#,
    )
    .bind(kind.as_str())
    .bind(video_id)
    .bind(JOB_STATUS_QUEUED)
    .bind(state.config.admin_job_max_attempts)
    .bind(JOB_STATUS_RUNNING)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    if result.rows_affected() > 0 {
        info!("Queued durable {:?} job for video {}", kind, video_id);
    } else {
        info!(
            "Durable {:?} job for video {} is already queued or running",
            kind, video_id
        );
    }
    Ok(())
}

async fn enqueue_catalog_publish_job(state: &AppState) -> Result<(), ApiError> {
    let result = sqlx::query(
        r#"
        INSERT INTO video_jobs (job_kind, video_id, status, max_attempts, run_after)
        SELECT $1, NULL::uuid, $2, $3, NOW()
        WHERE NOT EXISTS (
            SELECT 1 FROM video_jobs
            WHERE job_kind=$1
              AND status=$2
        )
        "#,
    )
    .bind(JOB_KIND_PUBLISH_CATALOG)
    .bind(JOB_STATUS_QUEUED)
    .bind(state.config.catalog_publish_job_max_attempts)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    if result.rows_affected() > 0 {
        info!("Queued durable catalog publish job");
    } else {
        info!("Durable catalog publish job is already queued");
    }
    Ok(())
}

async fn acquire_next_job(
    state: &AppState,
    worker_id: &str,
) -> Result<Option<LeasedJob>, ApiError> {
    let row = sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            attempts=attempts + 1,
            lease_owner=$2,
            lease_expires_at=NOW() + ($3::bigint * INTERVAL '1 second'),
            updated_at=NOW()
        WHERE id = (
            SELECT id
            FROM video_jobs
            WHERE (
                status=$4
                AND run_after <= NOW()
            ) OR (
                status=$1
                AND lease_expires_at IS NOT NULL
                AND lease_expires_at <= NOW()
            )
            ORDER BY run_after, created_at
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        RETURNING id, job_kind, video_id, attempts, max_attempts
        "#,
    )
    .bind(JOB_STATUS_RUNNING)
    .bind(worker_id)
    .bind(state.config.admin_job_lease_seconds)
    .bind(JOB_STATUS_QUEUED)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    let Some(row) = row else {
        return Ok(None);
    };
    let id: Uuid = row.try_get("id").map_err(db_error)?;
    let kind_raw: String = row.try_get("job_kind").map_err(db_error)?;
    let kind = JobKind::parse(&kind_raw).ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Unknown durable job kind {kind_raw:?}"),
        )
    })?;
    Ok(Some(LeasedJob {
        id,
        kind,
        video_id: row.try_get("video_id").map_err(db_error)?,
        attempts: row.try_get("attempts").map_err(db_error)?,
        max_attempts: row.try_get("max_attempts").map_err(db_error)?,
    }))
}

async fn run_leased_job(state: &AppState, job: &LeasedJob) -> Result<(), ApiError> {
    match job.kind {
        JobKind::ProcessVideo => run_process_video_job(state, job.video_id).await,
        JobKind::UploadVideo => run_upload_video_job(state, job.video_id).await,
        JobKind::PublishCatalog => run_catalog_publish_job(state).await,
    }
}

async fn run_process_video_job(state: &AppState, video_uuid: Option<Uuid>) -> Result<(), ApiError> {
    let video_uuid = video_uuid.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Process video job is missing video_id",
        )
    })?;
    let video_id = video_uuid.to_string();
    let row = sqlx::query(
        r#"
        SELECT status, job_dir, job_source_path, requested_resolutions
        FROM videos
        WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    let Some(row) = row else {
        info!("Skipping process job for deleted video {}", video_id);
        return Ok(());
    };
    let status: String = row.try_get("status").map_err(db_error)?;
    if !matches!(status.as_str(), STATUS_PENDING | STATUS_PROCESSING) {
        info!(
            "Skipping process job for video {} because status is {}",
            video_id, status
        );
        return Ok(());
    }

    let job_dir = row
        .try_get::<Option<String>, _>("job_dir")
        .map_err(db_error)?
        .map(PathBuf::from)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Processing job is missing job_dir",
            )
        })?;
    let resolutions =
        decode_requested_resolutions(row.try_get("requested_resolutions").map_err(db_error)?);
    let job_source_path: Option<String> = row.try_get("job_source_path").map_err(db_error)?;
    let source_path =
        recover_source_path(Some(&job_dir), job_source_path.as_deref()).ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Processing job is missing its source file",
            )
        })?;
    if resolutions.is_empty() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Processing job has no supported requested resolutions",
        ));
    }

    process_video_inner(state, &video_id, &source_path, &resolutions, &job_dir, true).await
}

async fn run_upload_video_job(state: &AppState, video_uuid: Option<Uuid>) -> Result<(), ApiError> {
    let video_uuid = video_uuid.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Upload video job is missing video_id",
        )
    })?;
    let video_id = video_uuid.to_string();
    let row = sqlx::query("SELECT status FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?;
    let Some(row) = row else {
        info!("Skipping upload job for deleted video {}", video_id);
        return Ok(());
    };
    let status: String = row.try_get("status").map_err(db_error)?;
    if status != STATUS_UPLOADING {
        info!(
            "Skipping upload job for video {} because status is {}",
            video_id, status
        );
        return Ok(());
    }

    upload_approved_video_inner(state, &video_id).await
}

async fn run_catalog_publish_job(state: &AppState) -> Result<(), ApiError> {
    let epoch = state.catalog_publish_epoch.load(Ordering::SeqCst);
    publish_current_catalog_to_network(state, epoch, "durable-job").await
}

async fn mark_job_succeeded(state: &AppState, job_id: Uuid) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            last_error=NULL,
            updated_at=NOW()
        WHERE id=$2
        "#,
    )
    .bind(JOB_STATUS_SUCCEEDED)
    .bind(job_id)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn mark_job_failed(state: &AppState, job: &LeasedJob, detail: &str) -> Result<(), ApiError> {
    let final_failure = job.attempts >= job.max_attempts;
    if final_failure {
        sqlx::query(
            r#"
            UPDATE video_jobs
            SET status=$1,
                lease_owner=NULL,
                lease_expires_at=NULL,
                last_error=$2,
                updated_at=NOW()
            WHERE id=$3
            "#,
        )
        .bind(JOB_STATUS_FAILED)
        .bind(detail)
        .bind(job.id)
        .execute(&state.pool)
        .await
        .map_err(db_error)?;
        handle_final_job_failure(state, job, detail).await;
        return Ok(());
    }

    let delay_seconds = job_retry_delay_seconds(job.attempts);
    let run_after = Utc::now() + Duration::seconds(delay_seconds);
    sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            run_after=$2,
            last_error=$3,
            updated_at=NOW()
        WHERE id=$4
        "#,
    )
    .bind(JOB_STATUS_QUEUED)
    .bind(run_after)
    .bind(detail)
    .bind(job.id)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    info!(
        "Retrying {:?} job {} in {}s after attempt {}/{}",
        job.kind, job.id, delay_seconds, job.attempts, job.max_attempts
    );
    Ok(())
}

async fn handle_final_job_failure(state: &AppState, job: &LeasedJob, detail: &str) {
    let Some(video_uuid) = job.video_id else {
        error!(
            "{:?} job {} failed permanently after {} attempt(s): {}",
            job.kind, job.id, job.attempts, detail
        );
        return;
    };
    let video_id = video_uuid.to_string();
    error!(
        "{:?} job {} for video {} failed permanently after {} attempt(s): {}",
        job.kind, job.id, video_id, job.attempts, detail
    );
    if matches!(job.kind, JobKind::ProcessVideo | JobKind::UploadVideo) {
        let _ = set_status(state, &video_id, STATUS_ERROR, Some(detail)).await;
        if let Ok(Some(job_dir)) = fetch_job_dir(state, &video_id).await {
            let _ = fs::remove_dir_all(job_dir);
        }
    }
}

fn job_retry_delay_seconds(attempts: i32) -> i64 {
    let exponent = attempts.saturating_sub(1).min(5) as u32;
    (30_i64 * 2_i64.pow(exponent)).min(15 * 60)
}

async fn process_video_inner(
    state: &AppState,
    video_id: &str,
    source_path: &FsPath,
    resolutions: &[String],
    job_dir: &FsPath,
    reset_existing: bool,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    if reset_existing {
        sqlx::query("DELETE FROM video_variants WHERE video_id=$1")
            .bind(video_uuid)
            .execute(&state.pool)
            .await
            .map_err(db_error)?;
        for resolution in resolutions {
            let _ = fs::remove_dir_all(job_dir.join(resolution));
        }
    }

    set_status(state, video_id, STATUS_PROCESSING, None).await?;
    let exists = sqlx::query("SELECT id FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?
        .is_some();
    if !exists {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Video row {video_id} disappeared before processing"),
        ));
    }

    let total_duration = probe_duration(source_path).await?.unwrap_or(0.0);
    let source_dimensions = probe_video_dimensions(source_path).await?;

    for resolution in resolutions {
        let Some((preset_width, preset_height, video_kbps, audio_kbps)) =
            resolution_preset(resolution)
        else {
            continue;
        };
        let (width, height) =
            target_dimensions_for_source(preset_width, preset_height, source_dimensions);
        let video_kbps =
            target_video_bitrate_kbps(video_kbps, preset_width, preset_height, width, height);
        let seg_dir = job_dir.join(resolution);
        fs::create_dir_all(&seg_dir).map_err(|err| {
            ApiError::new(
                StatusCode::INSUFFICIENT_STORAGE,
                format!("Could not create segment directory: {err}"),
            )
        })?;

        info!("Transcoding {} -> {}", video_id, resolution);
        run_ffmpeg(
            state,
            source_path,
            &seg_dir,
            width,
            height,
            video_kbps,
            audio_kbps,
        )
        .await?;

        let ts_files = collect_segment_files(&seg_dir)?;
        if ts_files.is_empty() {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("FFmpeg produced no segments for {resolution}"),
            ));
        }

        let variant_row = sqlx::query(
            r#"
            INSERT INTO video_variants
                (video_id, resolution, width, height, video_bitrate, audio_bitrate,
                 segment_duration, total_duration, segment_count)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
            RETURNING id
            "#,
        )
        .bind(video_uuid)
        .bind(resolution)
        .bind(width)
        .bind(height)
        .bind(video_kbps * 1000)
        .bind(audio_kbps * 1000)
        .bind(state.config.hls_segment_duration)
        .bind(total_duration)
        .bind(ts_files.len() as i32)
        .fetch_one(&state.pool)
        .await
        .map_err(db_error)?;
        let variant_id: Uuid = variant_row.try_get("id").map_err(db_error)?;

        for (idx, ts_path) in ts_files.iter().enumerate() {
            let duration = probe_duration(ts_path)
                .await?
                .unwrap_or(state.config.hls_segment_duration);
            let byte_size = fs::metadata(ts_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if byte_size < MIN_ANTD_SELF_ENCRYPTION_BYTES as u64 {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "FFmpeg produced a segment too small for Autonomi storage: {} ({} bytes)",
                        ts_path.display(),
                        byte_size
                    ),
                ));
            }
            sqlx::query(
                r#"
                INSERT INTO video_segments
                    (variant_id, segment_index, duration, byte_size, local_path)
                VALUES ($1,$2,$3,$4,$5)
                ON CONFLICT (variant_id, segment_index) DO UPDATE
                  SET duration=EXCLUDED.duration,
                      byte_size=EXCLUDED.byte_size,
                      local_path=EXCLUDED.local_path
                "#,
            )
            .bind(variant_id)
            .bind(idx as i32)
            .bind(duration)
            .bind(byte_size as i64)
            .bind(ts_path.to_string_lossy().as_ref())
            .execute(&state.pool)
            .await
            .map_err(db_error)?;
        }
    }

    let mut final_quote = build_final_upload_quote(state, video_id).await?;
    let expires_at = Utc::now() + Duration::seconds(state.config.final_quote_approval_ttl_seconds);
    final_quote["approval_expires_at"] = json!(expires_at.to_rfc3339());
    final_quote["quote_created_at"] = json!(Utc::now().to_rfc3339());
    set_awaiting_approval(state, video_id, final_quote, expires_at).await?;
    info!(
        "Video {} is awaiting approval expires_at={}",
        video_id,
        expires_at.to_rfc3339()
    );
    Ok(())
}

fn collect_segment_files(seg_dir: &FsPath) -> Result<Vec<PathBuf>, ApiError> {
    let mut files = fs::read_dir(seg_dir)
        .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("ts"))
        .filter(|path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|stem| stem.starts_with("seg_"))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|path| segment_index_from_path(path).unwrap_or(i32::MAX));
    Ok(files)
}

fn segment_index_from_path(path: &FsPath) -> Option<i32> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .and_then(|stem| stem.strip_prefix("seg_"))
        .and_then(|value| value.parse::<i32>().ok())
}

async fn build_final_upload_quote(state: &AppState, video_id: &str) -> Result<Value, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    #[derive(Default)]
    struct FinalVariantQuote {
        resolution: String,
        width: i32,
        height: i32,
        segment_count: i64,
        estimated_bytes: i64,
        actual_bytes: i64,
        chunk_count: i64,
        storage_cost_atto: u128,
        estimated_gas_cost_wei: u128,
        payment_mode: String,
    }
    struct FinalSegmentQuoteInput {
        order: usize,
        variant_id: Uuid,
        resolution: String,
        segment_index: i32,
        width: i32,
        height: i32,
        total_duration: Option<f64>,
        local_path: PathBuf,
    }
    struct FinalSegmentQuoteResult {
        order: usize,
        variant_id: Uuid,
        resolution: String,
        width: i32,
        height: i32,
        total_duration: Option<f64>,
        byte_size: i64,
        storage_cost: u128,
        gas_cost: u128,
        chunk_count: i64,
        payment_mode: String,
    }

    let video_row = sqlx::query(
        r#"
        SELECT upload_original, job_source_path
        FROM videos
        WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let upload_original = video_row.try_get("upload_original").unwrap_or(false);
    let original_source_path: Option<PathBuf> = video_row
        .try_get::<Option<String>, _>("job_source_path")
        .ok()
        .flatten()
        .map(PathBuf::from);

    let rows = sqlx::query(
        r#"
        SELECT v.id AS variant_id, v.resolution, v.width, v.height, v.total_duration,
               s.segment_index, s.local_path, s.byte_size
        FROM video_variants v
        JOIN video_segments s ON s.variant_id = v.id
        WHERE v.video_id=$1
        ORDER BY v.height DESC, s.segment_index
        "#,
    )
    .bind(video_uuid)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    if rows.is_empty() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "No transcoded segments were found for final quote",
        ));
    }

    let mut inputs = Vec::with_capacity(rows.len());
    for (order, row) in rows.iter().enumerate() {
        let local_path: Option<String> = row.try_get("local_path").ok().flatten();
        let path = local_path.as_deref().map(PathBuf::from).ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Transcoded segment is missing from disk",
            )
        })?;
        if !path.exists() {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Transcoded segment is missing from disk: {}",
                    path.display()
                ),
            ));
        }
        inputs.push(FinalSegmentQuoteInput {
            order,
            variant_id: row.try_get("variant_id").map_err(db_error)?,
            resolution: row.try_get("resolution").unwrap_or_default(),
            segment_index: row.try_get("segment_index").unwrap_or_default(),
            width: row.try_get("width").unwrap_or_default(),
            height: row.try_get("height").unwrap_or_default(),
            total_duration: row
                .try_get::<Option<f64>, _>("total_duration")
                .ok()
                .flatten(),
            local_path: path,
        });
    }

    let quote_started = Instant::now();
    let semaphore = Arc::new(Semaphore::new(state.config.antd_quote_concurrency));
    let mut jobs = JoinSet::new();
    for input in inputs {
        let antd = state.antd.clone();
        let semaphore = semaphore.clone();
        let default_payment_mode = state.config.antd_payment_mode.clone();
        jobs.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .map_err(|err| err.to_string())?;
            let metadata = tokio_fs::metadata(&input.local_path)
                .await
                .map_err(|err| format!("Could not inspect transcoded segment: {err}"))?;
            let byte_size = metadata.len();
            if byte_size < MIN_ANTD_SELF_ENCRYPTION_BYTES as u64 {
                return Err(format!(
                    "Transcoded segment is too small to store on Autonomi: {}/{}/segment-{:05} has {} bytes",
                    input.resolution,
                    input.variant_id,
                    input.segment_index,
                    byte_size
                ));
            }
            let estimate = antd
                .data_cost_for_size(byte_size as usize)
                .await
                .map_err(|err| {
                    format!(
                        "Could not get final Autonomi price quote for {}/segment-{:05} ({} bytes): {err}",
                        input.resolution, input.segment_index, byte_size
                    )
                })?;
            Ok::<FinalSegmentQuoteResult, String>(FinalSegmentQuoteResult {
                order: input.order,
                variant_id: input.variant_id,
                resolution: input.resolution,
                width: input.width,
                height: input.height,
                total_duration: input.total_duration,
                byte_size: byte_size as i64,
                storage_cost: parse_cost_u128(estimate.cost.as_deref()),
                gas_cost: parse_cost_u128(estimate.estimated_gas_cost_wei.as_deref()),
                chunk_count: estimate.chunk_count.unwrap_or(0),
                payment_mode: estimate.payment_mode.unwrap_or(default_payment_mode),
            })
        });
    }

    let mut results = Vec::with_capacity(rows.len());
    while let Some(joined) = jobs.join_next().await {
        let result = joined.map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Final quote task failed: {err}"),
            )
        })?;
        results.push(result.map_err(|err| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Could not get final Autonomi price quote: {err}"),
            )
        })?);
    }
    results.sort_by_key(|result| result.order);
    info!(
        "Final quote for {} checked {} segments in {:.2}s with concurrency={}",
        video_id,
        results.len(),
        quote_started.elapsed().as_secs_f64(),
        state.config.antd_quote_concurrency
    );

    let mut variants = Vec::<FinalVariantQuote>::new();
    let mut variant_indexes = HashMap::<String, usize>::new();
    let mut quote_cache = HashMap::<i64, QuoteValue>::new();
    let mut total_storage_cost = 0_u128;
    let mut total_gas_cost = 0_u128;
    let mut total_bytes = 0_i64;
    let mut total_chunks = 0_i64;
    let mut max_duration = 0.0_f64;
    let mut original_file_quote = None;

    for result in results {
        let variant_id = result.variant_id;
        let variant_key = variant_id.to_string();
        let index = *variant_indexes.entry(variant_key).or_insert_with(|| {
            variants.push(FinalVariantQuote {
                resolution: result.resolution.clone(),
                width: result.width,
                height: result.height,
                payment_mode: result.payment_mode.clone(),
                ..FinalVariantQuote::default()
            });
            variants.len() - 1
        });
        let variant = &mut variants[index];
        variant.segment_count += 1;
        variant.estimated_bytes += result.byte_size;
        variant.actual_bytes += result.byte_size;
        variant.chunk_count += result.chunk_count;
        variant.storage_cost_atto += result.storage_cost;
        variant.estimated_gas_cost_wei += result.gas_cost;

        total_storage_cost += result.storage_cost;
        total_gas_cost += result.gas_cost;
        total_bytes += result.byte_size;
        total_chunks += result.chunk_count;
        if let Some(duration) = result.total_duration {
            max_duration = max_duration.max(duration);
        }
    }
    let actual_transcoded_bytes = total_bytes;

    if upload_original {
        let path = original_source_path.ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Original source file is missing from disk",
            )
        })?;
        if !path.exists() {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Original source file is missing from disk: {}",
                    path.display()
                ),
            ));
        }
        let metadata = tokio_fs::metadata(&path).await.map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not inspect original source file: {err}"),
            )
        })?;
        let byte_size = metadata.len() as i64;
        let quote = quote_data_size(state, byte_size, &mut quote_cache)
            .await
            .map_err(|err| {
                ApiError::new(
                    err.status,
                    format!(
                        "Could not get final Autonomi price quote for original file: {}",
                        err.detail
                    ),
                )
            })?;
        let storage_cost = quote.storage_cost_atto;
        let gas_cost = quote.estimated_gas_cost_wei;
        let chunk_count = quote.chunk_count;
        let payment_mode = quote.payment_mode;
        total_storage_cost += storage_cost;
        total_gas_cost += gas_cost;
        total_bytes += byte_size;
        total_chunks += chunk_count;
        original_file_quote = Some(json!({
            "byte_size": byte_size,
            "chunk_count": chunk_count,
            "storage_cost_atto": storage_cost.to_string(),
            "estimated_gas_cost_wei": gas_cost.to_string(),
            "payment_mode": payment_mode,
        }));
    }

    let manifest_bytes = 4096 + (variants.len() as i64 * 1024) + (rows.len() as i64 * 220);
    let catalog_bytes = 2048 + (variants.len() as i64 * 512);
    let metadata_quote =
        quote_data_size(state, manifest_bytes + catalog_bytes, &mut quote_cache).await?;

    total_storage_cost += metadata_quote.storage_cost_atto;
    total_gas_cost += metadata_quote.estimated_gas_cost_wei;
    total_bytes += manifest_bytes + catalog_bytes;
    total_chunks += metadata_quote.chunk_count;

    let variant_values = variants
        .into_iter()
        .map(|variant| {
            json!({
                "resolution": variant.resolution,
                "width": variant.width,
                "height": variant.height,
                "segment_count": variant.segment_count,
                "estimated_bytes": variant.estimated_bytes,
                "actual_bytes": variant.actual_bytes,
                "chunk_count": variant.chunk_count,
                "storage_cost_atto": variant.storage_cost_atto.to_string(),
                "estimated_gas_cost_wei": variant.estimated_gas_cost_wei.to_string(),
                "payment_mode": variant.payment_mode,
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "quote_type": "final",
        "duration_seconds": max_duration,
        "segment_duration": state.config.hls_segment_duration,
        "payment_mode": state.config.antd_payment_mode.clone(),
        "estimated_bytes": total_bytes,
        "actual_media_bytes": total_bytes - (manifest_bytes + catalog_bytes),
        "actual_transcoded_bytes": actual_transcoded_bytes,
        "segment_count": rows.len(),
        "chunk_count": total_chunks,
        "storage_cost_atto": total_storage_cost.to_string(),
        "estimated_gas_cost_wei": total_gas_cost.to_string(),
        "metadata_bytes": manifest_bytes + catalog_bytes,
        "original_file": original_file_quote,
        "sampled": metadata_quote.sampled,
        "approval_ttl_seconds": state.config.final_quote_approval_ttl_seconds,
        "variants": variant_values,
    }))
}

async fn upload_approved_video_inner(state: &AppState, video_id: &str) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let video_row = sqlx::query(
        r#"
        SELECT title, original_filename, description, created_at, job_dir,
               job_source_path, show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size,
               original_file_autonomi_cost_atto, original_file_autonomi_payment_mode,
               publish_when_ready
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Video row {video_id} disappeared before upload"),
        )
    })?;

    let job_dir: Option<String> = video_row.try_get("job_dir").ok().flatten();
    let upload_original = video_row.try_get("upload_original").unwrap_or(false);
    let publish_when_ready = video_row.try_get("publish_when_ready").unwrap_or(false);
    let original_file = if upload_original {
        upload_original_file_if_needed(state, video_uuid, video_id, &video_row).await?
    } else {
        None
    };
    let mut manifest = json!({
        "schema_version": 1,
        "content_type": VIDEO_MANIFEST_CONTENT_TYPE,
        "id": video_id,
        "title": video_row.try_get::<String, _>("title").unwrap_or_default(),
        "original_filename": Value::Null,
        "description": video_row.try_get::<Option<String>, _>("description").ok().flatten(),
        "status": STATUS_READY,
        "created_at": video_row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        "updated_at": Utc::now().to_rfc3339(),
        "show_original_filename": false,
        "show_manifest_address": video_row.try_get::<bool, _>("show_manifest_address").unwrap_or(false),
        "original_file": original_file.unwrap_or(Value::Null),
        "variants": [],
    });

    let variants = sqlx::query(
        r#"
        SELECT id, resolution, width, height, video_bitrate, audio_bitrate,
               segment_duration, total_duration
        FROM video_variants
        WHERE video_id=$1
        ORDER BY height DESC
        "#,
    )
    .bind(video_uuid)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    struct SegmentUploadInput {
        segment_index: i32,
        local_path: PathBuf,
        label: String,
    }
    struct SegmentUploadResult {
        segment_index: i32,
        address: String,
        cost: Option<String>,
        payment_mode: String,
        byte_size: i64,
    }

    let mut manifest_variants = Vec::new();
    for variant in variants {
        let variant_id: Uuid = variant.try_get("id").map_err(db_error)?;
        let resolution = variant
            .try_get::<String, _>("resolution")
            .unwrap_or_default();
        let segment_rows = sqlx::query(
            r#"
            SELECT segment_index, local_path, duration, byte_size, autonomi_address
            FROM video_segments
            WHERE variant_id=$1
            ORDER BY segment_index
            "#,
        )
        .bind(variant_id)
        .fetch_all(&state.pool)
        .await
        .map_err(db_error)?;
        if segment_rows.is_empty() {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "No segments found for {}",
                    variant
                        .try_get::<String, _>("resolution")
                        .unwrap_or_default()
                ),
            ));
        }

        info!(
            "Uploading {} approved segments for {}/{} with payment_mode={} concurrency={}",
            segment_rows.len(),
            video_id,
            resolution,
            state.config.antd_payment_mode,
            state.config.antd_upload_concurrency
        );
        let mut upload_inputs = Vec::new();
        for segment in &segment_rows {
            let existing_address: Option<String> =
                segment.try_get("autonomi_address").ok().flatten();
            if existing_address.is_some() {
                continue;
            }
            let local_path: Option<String> = segment.try_get("local_path").ok().flatten();
            let path = local_path.as_deref().map(PathBuf::from).ok_or_else(|| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Transcoded segment is missing from disk",
                )
            })?;
            if !path.exists() {
                return Err(ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!(
                        "Transcoded segment is missing from disk: {}",
                        path.display()
                    ),
                ));
            }
            let segment_index = segment
                .try_get::<i32, _>("segment_index")
                .unwrap_or_default();
            upload_inputs.push(SegmentUploadInput {
                segment_index,
                local_path: path,
                label: format!("{}/{}/segment-{segment_index:05}", video_id, resolution),
            });
        }

        let upload_started = Instant::now();
        let semaphore = Arc::new(Semaphore::new(state.config.antd_upload_concurrency));
        let mut jobs = JoinSet::new();
        for input in upload_inputs {
            let antd = state.antd.clone();
            let semaphore = semaphore.clone();
            let payment_mode = state.config.antd_payment_mode.clone();
            let upload_verify = state.config.antd_upload_verify;
            let upload_retries = state.config.antd_upload_retries;
            let direct_upload_max_bytes = state.config.antd_direct_upload_max_bytes;
            jobs.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|err| err.to_string())?;
                let metadata = tokio_fs::metadata(&input.local_path)
                    .await
                    .map_err(|err| format!("Could not inspect transcoded segment: {err}"))?;
                let byte_size = metadata.len() as i64;
                if metadata.len() < MIN_ANTD_SELF_ENCRYPTION_BYTES as u64 {
                    return Err(format!(
                        "Transcoded segment is too small to store on Autonomi: {} has {} bytes",
                        input.label, byte_size
                    ));
                }
                match antd
                    .file_put_public(&input.local_path, &payment_mode, upload_verify)
                    .await
                {
                    Ok(result) => Ok::<SegmentUploadResult, String>(SegmentUploadResult {
                        segment_index: input.segment_index,
                        address: result.address,
                        cost: Some(result.storage_cost_atto),
                        payment_mode: result.payment_mode_used,
                        byte_size: result.byte_size as i64,
                    }),
                    Err(err) if is_missing_file_upload_endpoint(&err) => {
                        if metadata.len() as usize > direct_upload_max_bytes {
                            return Err(format!(
                                "Autonomi file upload endpoint is unavailable and legacy JSON upload for {} would exceed ANTD_DIRECT_UPLOAD_MAX_BYTES ({})",
                                input.label,
                                direct_upload_max_bytes
                            ));
                        }
                        let data = tokio_fs::read(&input.local_path)
                            .await
                            .map_err(|err| format!("Could not read transcoded segment: {err}"))?;
                        let result = put_public_verified_inner(
                            antd,
                            payment_mode.clone(),
                            upload_verify,
                            upload_retries,
                            data,
                            input.label,
                        )
                        .await?;
                        Ok(SegmentUploadResult {
                            segment_index: input.segment_index,
                            address: result.address,
                            cost: result.cost,
                            payment_mode,
                            byte_size,
                        })
                    }
                    Err(err) => Err(format!(
                        "Autonomi file upload failed for {}: {err}",
                        input.label
                    )),
                }
            });
        }

        let mut uploaded_results = Vec::new();
        while let Some(joined) = jobs.join_next().await {
            let result = joined.map_err(|err| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Segment upload task failed: {err}"),
                )
            })?;
            uploaded_results.push(result.map_err(|err| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Autonomi segment upload failed: {err}"),
                )
            })?);
        }
        uploaded_results.sort_by_key(|result| result.segment_index);
        if !uploaded_results.is_empty() {
            info!(
                "Uploaded {} segments for {}/{} in {:.2}s",
                uploaded_results.len(),
                video_id,
                resolution,
                upload_started.elapsed().as_secs_f64()
            );
        }

        for result in uploaded_results {
            sqlx::query(
                r#"
                UPDATE video_segments
                SET autonomi_address=$1,
                    autonomi_cost_atto=$2,
                    autonomi_payment_mode=$3,
                    byte_size=$4
                WHERE variant_id=$5 AND segment_index=$6
                "#,
            )
            .bind(&result.address)
            .bind(result.cost.as_deref())
            .bind(&result.payment_mode)
            .bind(result.byte_size)
            .bind(variant_id)
            .bind(result.segment_index)
            .execute(&state.pool)
            .await
            .map_err(db_error)?;
        }

        let uploaded_segments = sqlx::query(
            r#"
            SELECT segment_index, autonomi_address, duration, byte_size
            FROM video_segments
            WHERE variant_id=$1
            ORDER BY segment_index
            "#,
        )
        .bind(variant_id)
        .fetch_all(&state.pool)
        .await
        .map_err(db_error)?;

        manifest_variants.push(json!({
            "id": variant_id.to_string(),
            "resolution": variant.try_get::<String, _>("resolution").unwrap_or_default(),
            "width": variant.try_get::<i32, _>("width").unwrap_or_default(),
            "height": variant.try_get::<i32, _>("height").unwrap_or_default(),
            "video_bitrate": variant.try_get::<i32, _>("video_bitrate").unwrap_or_default(),
            "audio_bitrate": variant.try_get::<i32, _>("audio_bitrate").unwrap_or_default(),
            "segment_duration": variant.try_get::<f64, _>("segment_duration").unwrap_or_default(),
            "total_duration": variant.try_get::<Option<f64>, _>("total_duration").ok().flatten(),
            "segment_count": uploaded_segments.len(),
            "segments": uploaded_segments
                .iter()
                .map(|segment| {
                    json!({
                        "segment_index": segment.try_get::<i32, _>("segment_index").unwrap_or_default(),
                        "autonomi_address": segment.try_get::<Option<String>, _>("autonomi_address").ok().flatten(),
                        "duration": segment.try_get::<f64, _>("duration").unwrap_or_default(),
                        "byte_size": segment.try_get::<Option<i64>, _>("byte_size").ok().flatten(),
                    })
                })
                .collect::<Vec<_>>(),
        }));
    }

    manifest["updated_at"] = json!(Utc::now().to_rfc3339());
    manifest["variants"] = json!(manifest_variants);
    let manifest_address = store_json_public(state, &manifest).await?;
    let catalog_address = read_catalog_address(&state.config);
    set_ready(
        state,
        video_id,
        &manifest_address,
        catalog_address.as_deref(),
    )
    .await?;
    let mut is_public = false;
    if publish_when_ready {
        set_publication(
            state,
            video_id,
            true,
            Some(&manifest_address),
            catalog_address.as_deref(),
        )
        .await?;
        let epoch = refresh_local_catalog_from_db(state, "auto-publish").await?;
        schedule_catalog_publish(state, epoch, format!("auto-publish:{video_id}")).await?;
        is_public = true;
    }
    if let Some(job_dir) = job_dir {
        let _ = fs::remove_dir_all(job_dir);
    }
    info!(
        "Video {} is ready manifest={} catalog={:?} public={}",
        video_id, manifest_address, catalog_address, is_public
    );
    Ok(())
}

async fn upload_original_file_if_needed(
    state: &AppState,
    video_uuid: Uuid,
    video_id: &str,
    video_row: &sqlx::postgres::PgRow,
) -> Result<Option<Value>, ApiError> {
    if let Some(address) = video_row
        .try_get::<Option<String>, _>("original_file_address")
        .ok()
        .flatten()
    {
        return Ok(Some(json!({
            "autonomi_address": address,
            "byte_size": video_row
                .try_get::<Option<i64>, _>("original_file_byte_size")
                .ok()
                .flatten(),
            "autonomi_cost_atto": video_row
                .try_get::<Option<String>, _>("original_file_autonomi_cost_atto")
                .ok()
                .flatten(),
            "payment_mode": video_row
                .try_get::<Option<String>, _>("original_file_autonomi_payment_mode")
                .ok()
                .flatten(),
        })));
    }

    let source_path = video_row
        .try_get::<Option<String>, _>("job_source_path")
        .ok()
        .flatten()
        .map(PathBuf::from)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Original source file is missing from disk",
            )
        })?;
    if !source_path.exists() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Original source file is missing from disk: {}",
                source_path.display()
            ),
        ));
    }

    let metadata = tokio_fs::metadata(&source_path).await.map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not inspect original source file: {err}"),
        )
    })?;
    let byte_size = metadata.len() as i64;
    let filename = video_row
        .try_get::<String, _>("original_filename")
        .unwrap_or_else(|_| "source".to_string());
    let upload_label = format!("{video_id}/original/{filename}");
    let file_result = state
        .antd
        .file_put_public(
            &source_path,
            &state.config.antd_payment_mode,
            state.config.antd_upload_verify,
        )
        .await;
    let (address, cost, payment_mode) = match file_result {
        Ok(result) => (
            result.address,
            Some(result.storage_cost_atto),
            result.payment_mode_used,
        ),
        Err(err) if is_missing_file_upload_endpoint(&err) => {
            if metadata.len() as usize > state.config.antd_direct_upload_max_bytes {
                warn!(
                    label = %upload_label,
                    byte_size = metadata.len(),
                    direct_upload_max_bytes = state.config.antd_direct_upload_max_bytes,
                    "Skipping optional original source upload because the Autonomi file upload endpoint is unavailable and legacy JSON upload would exceed the configured cap"
                );
                return Ok(None);
            }
            let data = tokio_fs::read(&source_path).await.map_err(|err| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Could not read original source file: {err}"),
                )
            })?;
            let result = put_public_verified_with_mode(
                state,
                &data,
                &upload_label,
                &state.config.antd_payment_mode,
            )
            .await?;
            (
                result.address,
                result.cost,
                state.config.antd_payment_mode.clone(),
            )
        }
        Err(err) => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Autonomi original source upload failed for {upload_label}: {err}"),
            ));
        }
    };
    sqlx::query(
        r#"
        UPDATE videos
        SET original_file_address=$1,
            original_file_byte_size=$2,
            original_file_autonomi_cost_atto=$3,
            original_file_autonomi_payment_mode=$4,
            updated_at=NOW()
        WHERE id=$5
        "#,
    )
    .bind(&address)
    .bind(byte_size)
    .bind(cost.as_deref())
    .bind(&payment_mode)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;

    Ok(Some(json!({
        "autonomi_address": address,
        "byte_size": byte_size,
        "autonomi_cost_atto": cost,
        "payment_mode": payment_mode,
    })))
}

fn original_file_manifest_from_row(row: &sqlx::postgres::PgRow) -> Option<Value> {
    let address = row
        .try_get::<Option<String>, _>("original_file_address")
        .ok()
        .flatten()?;
    Some(json!({
        "autonomi_address": address,
        "byte_size": row
            .try_get::<Option<i64>, _>("original_file_byte_size")
            .ok()
            .flatten(),
        "autonomi_cost_atto": row
            .try_get::<Option<String>, _>("original_file_autonomi_cost_atto")
            .ok()
            .flatten(),
        "payment_mode": row
            .try_get::<Option<String>, _>("original_file_autonomi_payment_mode")
            .ok()
            .flatten(),
    }))
}

async fn build_ready_manifest_from_db(state: &AppState, video_id: &str) -> Result<Value, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let video_row = sqlx::query(
        r#"
        SELECT title, original_filename, description, created_at,
               show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size,
               original_file_autonomi_cost_atto, original_file_autonomi_payment_mode
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let variants = sqlx::query(
        r#"
        SELECT id, resolution, width, height, video_bitrate, audio_bitrate,
               segment_duration, total_duration
        FROM video_variants
        WHERE video_id=$1
        ORDER BY height DESC
        "#,
    )
    .bind(video_uuid)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let mut manifest_variants = Vec::new();
    for variant in variants {
        let variant_id: Uuid = variant.try_get("id").map_err(db_error)?;
        let uploaded_segments = sqlx::query(
            r#"
            SELECT segment_index, autonomi_address, duration, byte_size
            FROM video_segments
            WHERE variant_id=$1
            ORDER BY segment_index
            "#,
        )
        .bind(variant_id)
        .fetch_all(&state.pool)
        .await
        .map_err(db_error)?;
        if uploaded_segments.iter().any(|segment| {
            segment
                .try_get::<Option<String>, _>("autonomi_address")
                .ok()
                .flatten()
                .is_none()
        }) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "Video has not finished uploading all segment addresses",
            ));
        }
        manifest_variants.push(json!({
            "id": variant_id.to_string(),
            "resolution": variant.try_get::<String, _>("resolution").unwrap_or_default(),
            "width": variant.try_get::<i32, _>("width").unwrap_or_default(),
            "height": variant.try_get::<i32, _>("height").unwrap_or_default(),
            "video_bitrate": variant.try_get::<i32, _>("video_bitrate").unwrap_or_default(),
            "audio_bitrate": variant.try_get::<i32, _>("audio_bitrate").unwrap_or_default(),
            "segment_duration": variant.try_get::<f64, _>("segment_duration").unwrap_or_default(),
            "total_duration": variant.try_get::<Option<f64>, _>("total_duration").ok().flatten(),
            "segment_count": uploaded_segments.len(),
            "segments": uploaded_segments
                .iter()
                .map(|segment| {
                    json!({
                        "segment_index": segment.try_get::<i32, _>("segment_index").unwrap_or_default(),
                        "autonomi_address": segment.try_get::<Option<String>, _>("autonomi_address").ok().flatten(),
                        "duration": segment.try_get::<f64, _>("duration").unwrap_or_default(),
                        "byte_size": segment.try_get::<Option<i64>, _>("byte_size").ok().flatten(),
                    })
                })
                .collect::<Vec<_>>(),
        }));
    }

    Ok(json!({
        "schema_version": 1,
        "content_type": VIDEO_MANIFEST_CONTENT_TYPE,
        "id": video_id,
        "title": video_row.try_get::<String, _>("title").unwrap_or_default(),
        "original_filename": Value::Null,
        "description": video_row.try_get::<Option<String>, _>("description").ok().flatten(),
        "status": STATUS_READY,
        "created_at": video_row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        "updated_at": Utc::now().to_rfc3339(),
        "show_original_filename": false,
        "show_manifest_address": video_row
            .try_get::<bool, _>("show_manifest_address")
            .unwrap_or(false),
        "original_file": original_file_manifest_from_row(&video_row).unwrap_or(Value::Null),
        "variants": manifest_variants,
    }))
}

async fn build_catalog_entry_from_db(
    state: &AppState,
    video_id: &str,
    manifest_address: String,
) -> Result<Value, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let video_row = sqlx::query(
        r#"
        SELECT title, original_filename, description, created_at,
               show_original_filename, show_manifest_address,
               upload_original, original_file_address, original_file_byte_size
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_uuid)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?
    .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let variant_rows = sqlx::query(
        r#"
        SELECT resolution, width, height, total_duration, segment_count
        FROM video_variants
        WHERE video_id=$1
        ORDER BY height DESC
        "#,
    )
    .bind(video_uuid)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let input = CatalogEntryInput {
        video_id: video_id.to_string(),
        title: video_row.try_get("title").unwrap_or_default(),
        description: video_row.try_get("description").ok().flatten(),
        created_at: video_row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|_| Utc::now().to_rfc3339()),
        updated_at: Utc::now().to_rfc3339(),
        manifest_address,
        show_manifest_address: video_row
            .try_get::<bool, _>("show_manifest_address")
            .unwrap_or(false),
        variants: variant_rows
            .iter()
            .map(|variant| {
                json!({
                    "resolution": variant.try_get::<String, _>("resolution").unwrap_or_default(),
                    "width": variant.try_get::<i32, _>("width").unwrap_or_default(),
                    "height": variant.try_get::<i32, _>("height").unwrap_or_default(),
                    "segment_count": variant.try_get::<Option<i32>, _>("segment_count").ok().flatten().unwrap_or(0),
                    "total_duration": variant.try_get::<Option<f64>, _>("total_duration").ok().flatten(),
                })
            })
            .collect(),
    };
    Ok(video_catalog_entry_from_input(input))
}

fn video_catalog_entry_from_input(input: CatalogEntryInput) -> Value {
    json!({
        "id": input.video_id,
        "title": input.title,
        "original_filename": Value::Null,
        "description": input.description,
        "status": STATUS_READY,
        "created_at": input.created_at,
        "updated_at": input.updated_at,
        "manifest_address": input.manifest_address,
        "show_original_filename": false,
        "show_manifest_address": input.show_manifest_address,
        "variants": input.variants,
    })
}

async fn ensure_video_manifest_address(
    state: &AppState,
    video_id: &str,
) -> Result<String, ApiError> {
    let existing_manifest_address = sqlx::query("SELECT manifest_address FROM videos WHERE id=$1")
        .bind(parse_video_uuid(video_id)?)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?
        .try_get::<Option<String>, _>("manifest_address")
        .ok()
        .flatten();

    if let Some(address) = existing_manifest_address {
        return Ok(address);
    }

    let manifest = build_ready_manifest_from_db(state, video_id).await?;
    let manifest_address = store_json_public(state, &manifest).await?;
    sqlx::query("UPDATE videos SET manifest_address=$1, updated_at=NOW() WHERE id=$2")
        .bind(&manifest_address)
        .bind(parse_video_uuid(video_id)?)
        .execute(&state.pool)
        .await
        .map_err(db_error)?;
    Ok(manifest_address)
}

async fn build_public_catalog_from_db(state: &AppState) -> Result<Value, ApiError> {
    let rows = sqlx::query(
        r#"
        SELECT id, manifest_address
        FROM videos
        WHERE status=$1
          AND is_public=TRUE
          AND manifest_address IS NOT NULL
        ORDER BY updated_at DESC NULLS LAST, created_at DESC NULLS LAST
        "#,
    )
    .bind(STATUS_READY)
    .fetch_all(&state.pool)
    .await
    .map_err(db_error)?;

    let mut videos = Vec::with_capacity(rows.len());
    for row in rows {
        let video_id: Uuid = row.try_get("id").map_err(db_error)?;
        let Some(manifest_address) = row
            .try_get::<Option<String>, _>("manifest_address")
            .ok()
            .flatten()
        else {
            continue;
        };
        videos.push(
            build_catalog_entry_from_db(state, &video_id.to_string(), manifest_address).await?,
        );
    }

    Ok(json!({
        "schema_version": 1,
        "content_type": CATALOG_CONTENT_TYPE,
        "updated_at": Utc::now().to_rfc3339(),
        "videos": videos,
    }))
}

async fn refresh_local_catalog_from_db(state: &AppState, reason: &str) -> Result<u64, ApiError> {
    let catalog = build_public_catalog_from_db(state).await?;
    let video_count = catalog
        .get("videos")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let _guard = state.catalog_lock.lock().await;
    let epoch = state.catalog_publish_epoch.fetch_add(1, Ordering::SeqCst) + 1;
    let catalog_address = read_catalog_address(&state.config);
    write_catalog_state(
        &state.config,
        catalog_address.as_deref(),
        Some(&catalog),
        true,
    )?;
    info!(
        "Queued local catalog update epoch={} reason={} videos={}",
        epoch, reason, video_count
    );
    Ok(epoch)
}

async fn schedule_catalog_publish(
    state: &AppState,
    epoch: u64,
    reason: impl Into<String>,
) -> Result<(), ApiError> {
    let reason = reason.into();
    enqueue_catalog_publish_job(state).await?;
    info!(
        "Queued durable catalog publish epoch={} reason={}",
        epoch, reason
    );
    Ok(())
}

async fn publish_current_catalog_to_network(
    state: &AppState,
    epoch: u64,
    reason: &str,
) -> Result<(), ApiError> {
    sleep(StdDuration::from_millis(250)).await;
    if state.catalog_publish_epoch.load(Ordering::SeqCst) != epoch {
        info!(
            "Skipping stale catalog publish epoch={} reason={}",
            epoch, reason
        );
        return Ok(());
    }

    let _publish_guard = state.catalog_publish_lock.lock().await;
    if state.catalog_publish_epoch.load(Ordering::SeqCst) != epoch {
        info!(
            "Skipping stale catalog publish epoch={} reason={}",
            epoch, reason
        );
        return Ok(());
    }

    let catalog = build_public_catalog_from_db(state).await?;
    let video_count = catalog
        .get("videos")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let start = Instant::now();
    let catalog_address = store_json_public(state, &catalog).await?;

    let _state_guard = state.catalog_lock.lock().await;
    if state.catalog_publish_epoch.load(Ordering::SeqCst) != epoch {
        info!(
            "Discarding stale catalog publish result epoch={} reason={} address={}",
            epoch, reason, catalog_address
        );
        return Ok(());
    }

    write_catalog_state(&state.config, Some(&catalog_address), Some(&catalog), false)?;
    set_current_catalog_address(state, &catalog_address).await?;
    info!(
        "Published catalog epoch={} reason={} videos={} address={} in {:.2}s",
        epoch,
        reason,
        video_count,
        catalog_address,
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

async fn store_json_public(state: &AppState, payload: &Value) -> Result<String, ApiError> {
    let data = serde_json::to_vec(payload).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not encode JSON document: {err}"),
        )
    })?;
    let result = put_public_verified_with_mode(
        state,
        &data,
        "json document",
        &state.config.antd_metadata_payment_mode,
    )
    .await?;
    Ok(result.address)
}

async fn put_public_verified_with_mode(
    state: &AppState,
    data: &[u8],
    label: &str,
    payment_mode: &str,
) -> Result<AntdDataPutResponse, ApiError> {
    if data.len() > state.config.antd_direct_upload_max_bytes {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "Direct JSON upload for {label} is {} but ANTD_DIRECT_UPLOAD_MAX_BYTES is {}; media uploads must use the streaming file endpoint",
                format_bytes(data.len() as u64),
                format_bytes(state.config.antd_direct_upload_max_bytes as u64)
            ),
        ));
    }
    put_public_verified_inner(
        state.antd.clone(),
        payment_mode.to_string(),
        state.config.antd_upload_verify,
        state.config.antd_upload_retries,
        data.to_vec(),
        label.to_string(),
    )
    .await
    .map_err(|err| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err))
}

async fn put_public_verified_inner(
    antd: AntdRestClient,
    payment_mode: String,
    upload_verify: bool,
    upload_retries: usize,
    data: Vec<u8>,
    label: String,
) -> Result<AntdDataPutResponse, String> {
    let mut last_error = None;
    for attempt in 1..=upload_retries {
        info!(
            "Uploading {} ({} bytes), attempt {}/{}",
            label,
            data.len(),
            attempt,
            upload_retries
        );
        let result = antd.data_put_public(&data, &payment_mode).await;
        match result {
            Ok(result) => {
                if upload_verify {
                    match antd.data_get_public(&result.address).await {
                        Ok(retrieved) if retrieved == data.as_slice() => return Ok(result),
                        Ok(retrieved) => {
                            last_error = Some(format!(
                                "Autonomi verification mismatch for {label}: stored {} bytes, retrieved {} bytes",
                                data.len(),
                                retrieved.len()
                            ));
                        }
                        Err(err) => last_error = Some(err.to_string()),
                    }
                } else {
                    return Ok(result);
                }
            }
            Err(err) => last_error = Some(err.to_string()),
        }

        if attempt < upload_retries {
            let delay = 2_u64.pow((attempt - 1).min(3) as u32);
            warn!(
                "Autonomi upload verification failed for {} on attempt {}/{}: {}; retrying in {}s",
                label,
                attempt,
                upload_retries,
                last_error.as_deref().unwrap_or("unknown error"),
                delay
            );
            sleep(StdDuration::from_secs(delay)).await;
        }
    }

    Err(format!(
        "Autonomi upload failed verification for {} after {} attempt(s): {}",
        label,
        upload_retries,
        last_error.unwrap_or_else(|| "unknown error".to_string())
    ))
}

fn write_catalog_state(
    config: &Config,
    address: Option<&str>,
    catalog: Option<&Value>,
    publish_pending: bool,
) -> Result<(), ApiError> {
    if let Some(parent) = config.catalog_state_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not create catalog state directory: {err}"),
            )
        })?;
    }
    let tmp_path = config.catalog_state_path.with_extension("tmp");
    let mut payload = json!({
        "catalog_address": address.unwrap_or(""),
        "updated_at": Utc::now().to_rfc3339(),
        "publish_pending": publish_pending,
        "note": "Local catalog snapshot plus the latest network-hosted catalog address.",
    });
    if let Some(catalog) = catalog {
        payload["catalog"] = catalog.clone();
    }
    fs::write(
        &tmp_path,
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not write catalog state: {err}"),
        )
    })?;
    fs::rename(&tmp_path, &config.catalog_state_path).map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not update catalog state: {err}"),
        )
    })
}

async fn set_status(
    state: &AppState,
    video_id: &str,
    status: &str,
    error_message: Option<&str>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    sqlx::query(
        r#"
        UPDATE videos
        SET status=$1, error_message=$2, updated_at=NOW()
        WHERE id=$3
        "#,
    )
    .bind(status)
    .bind(error_message)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn set_awaiting_approval(
    state: &AppState,
    video_id: &str,
    final_quote: Value,
    expires_at: DateTime<Utc>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    sqlx::query(
        r#"
        UPDATE videos
        SET status='awaiting_approval',
            final_quote=$1::jsonb,
            final_quote_created_at=NOW(),
            approval_expires_at=$2,
            error_message=NULL,
            updated_at=NOW()
        WHERE id=$3
        "#,
    )
    .bind(final_quote)
    .bind(expires_at)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn set_ready(
    state: &AppState,
    video_id: &str,
    manifest_address: &str,
    catalog_address: Option<&str>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    sqlx::query(
        r#"
        UPDATE videos
        SET status='ready',
            manifest_address=$1,
            catalog_address=$2,
            is_public=FALSE,
            error_message=NULL,
            job_dir=NULL,
            job_source_path=NULL,
            approval_expires_at=NULL,
            updated_at=NOW()
        WHERE id=$3
        "#,
    )
    .bind(manifest_address)
    .bind(catalog_address)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn set_publication(
    state: &AppState,
    video_id: &str,
    is_public: bool,
    manifest_address: Option<&str>,
    catalog_address: Option<&str>,
) -> Result<(), ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    sqlx::query(
        r#"
        UPDATE videos
        SET is_public=$1,
            manifest_address=COALESCE($2, manifest_address),
            catalog_address=COALESCE($3, catalog_address),
            updated_at=NOW()
        WHERE id=$4
        "#,
    )
    .bind(is_public)
    .bind(manifest_address)
    .bind(catalog_address)
    .bind(video_uuid)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn set_current_catalog_address(
    state: &AppState,
    catalog_address: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        r#"
        UPDATE videos
        SET catalog_address=$1
        WHERE status=$2
        "#,
    )
    .bind(catalog_address)
    .bind(STATUS_READY)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

async fn fetch_job_dir(state: &AppState, video_id: &str) -> Result<Option<String>, ApiError> {
    let video_uuid = parse_video_uuid(video_id)?;
    let row = sqlx::query("SELECT job_dir FROM videos WHERE id=$1")
        .bind(video_uuid)
        .fetch_optional(&state.pool)
        .await
        .map_err(db_error)?;
    Ok(row.and_then(|row| row.try_get::<Option<String>, _>("job_dir").ok().flatten()))
}

async fn cleanup_expired_approvals(state: &AppState) -> anyhow::Result<()> {
    let rows = sqlx::query(
        r#"
        UPDATE videos
        SET status='expired',
            error_message='Final quote approval window expired; local files were deleted.',
            updated_at=NOW()
        WHERE status='awaiting_approval'
          AND approval_expires_at IS NOT NULL
          AND approval_expires_at <= NOW()
        RETURNING id, job_dir
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    for row in rows {
        if let Ok(Some(job_dir)) = row.try_get::<Option<String>, _>("job_dir") {
            let _ = fs::remove_dir_all(job_dir);
        }
        if let Ok(video_id) = row.try_get::<Uuid, _>("id") {
            info!(
                "Expired awaiting approval video {} and removed local files",
                video_id
            );
        }
    }
    Ok(())
}

async fn approval_cleanup_loop(state: AppState) {
    loop {
        sleep(StdDuration::from_secs(
            state.config.approval_cleanup_interval_seconds,
        ))
        .await;
        if let Err(err) = cleanup_expired_approvals(&state).await {
            warn!("Approval cleanup failed: {}", err);
        }
    }
}

async fn recover_interrupted_jobs(state: AppState) -> anyhow::Result<()> {
    let reset_jobs = sqlx::query(
        r#"
        UPDATE video_jobs
        SET status=$1,
            lease_owner=NULL,
            lease_expires_at=NULL,
            run_after=NOW(),
            updated_at=NOW()
        WHERE status=$2
        "#,
    )
    .bind(JOB_STATUS_QUEUED)
    .bind(JOB_STATUS_RUNNING)
    .execute(&state.pool)
    .await?;
    if reset_jobs.rows_affected() > 0 {
        info!(
            "Requeued {} running durable job(s) from a previous admin process",
            reset_jobs.rows_affected()
        );
    }

    let rows = sqlx::query(
        r#"
        SELECT id, status, job_dir, job_source_path, requested_resolutions
        FROM videos
        WHERE status IN ('pending', 'processing', 'uploading')
        ORDER BY created_at
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let mut recovered_processing = 0;
    let mut recovered_uploads = 0;
    for row in rows {
        let video_id: Uuid = row.try_get("id")?;
        let video_id = video_id.to_string();
        let status: String = row.try_get("status")?;
        let job_dir = row
            .try_get::<Option<String>, _>("job_dir")?
            .map(PathBuf::from);

        if matches!(status.as_str(), STATUS_PENDING | STATUS_PROCESSING) {
            let resolutions = decode_requested_resolutions(
                row.try_get::<Option<Value>, _>("requested_resolutions")?,
            );
            let source_path = recover_source_path(
                job_dir.as_deref(),
                row.try_get::<Option<String>, _>("job_source_path")?
                    .as_deref(),
            );
            if job_dir.as_ref().is_none_or(|path| !path.exists())
                || source_path.is_none()
                || resolutions.is_empty()
            {
                let _ = set_status(
                    &state,
                    &video_id,
                    STATUS_ERROR,
                    Some(
                        "Interrupted processing job could not be recovered because its source file or requested resolutions were missing.",
                    ),
                )
                .await;
                warn!("Could not recover interrupted processing job {}", video_id);
                continue;
            }

            schedule_processing_job(&state, &video_id)
                .await
                .map_err(|err| anyhow::anyhow!(err.detail))?;
            recovered_processing += 1;
        } else if status == STATUS_UPLOADING {
            schedule_upload_job(&state, &video_id)
                .await
                .map_err(|err| anyhow::anyhow!(err.detail))?;
            recovered_uploads += 1;
        }
    }

    if recovered_processing > 0 || recovered_uploads > 0 {
        info!(
            "Recovered interrupted jobs: processing={} uploading={}",
            recovered_processing, recovered_uploads
        );
    }
    if read_catalog_state_value(&state.config)
        .and_then(|value| value.get("publish_pending").and_then(Value::as_bool))
        .unwrap_or(false)
    {
        enqueue_catalog_publish_job(&state)
            .await
            .map_err(|err| anyhow::anyhow!(err.detail))?;
        info!("Recovered pending catalog publish from local catalog state");
    }
    Ok(())
}

fn decode_requested_resolutions(value: Option<Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .filter(|item| resolution_preset(item).is_some())
            .collect(),
        Some(Value::String(value)) => parse_resolutions(&value),
        _ => vec![],
    }
}

fn recover_source_path(job_dir: Option<&FsPath>, job_source_path: Option<&str>) -> Option<PathBuf> {
    if let Some(source_path) = job_source_path
        .map(PathBuf::from)
        .filter(|path| path.exists())
    {
        return Some(source_path);
    }

    let job_dir = job_dir?;
    let mut matches = fs::read_dir(job_dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("original_"))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.into_iter().next()
}

fn string_field(value: &Value, key: &str) -> String {
    opt_string_field(value, key).unwrap_or_default()
}

fn opt_string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn int_field(value: &Value, key: &str) -> i32 {
    value.get(key).and_then(Value::as_i64).unwrap_or_default() as i32
}

fn db_error(err: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

fn parse_video_uuid(video_id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(video_id).map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))
}

fn resolution_preset(resolution: &str) -> Option<(i32, i32, i32, i32)> {
    match resolution {
        "8k" => Some((7680, 4320, 45000, 320)),
        "4k" => Some((3840, 2160, 16000, 256)),
        "1440p" => Some((2560, 1440, 8000, 192)),
        "1080p" => Some((1920, 1080, 5000, 192)),
        "720p" => Some((1280, 720, 2500, 128)),
        "540p" => Some((960, 540, 1600, 128)),
        "480p" => Some((854, 480, 1000, 128)),
        "360p" => Some((640, 360, 500, 96)),
        "240p" => Some((426, 240, 300, 64)),
        "144p" => Some((256, 144, 150, 48)),
        _ => None,
    }
}

fn supported_resolutions_error() -> String {
    let values = SUPPORTED_RESOLUTIONS
        .iter()
        .map(|resolution| format!("'{resolution}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("No valid resolutions. Choose from: [{values}]")
}

fn even_floor(value: f64) -> i32 {
    let floored = value.floor().max(2.0) as i32;
    let even = floored - floored.rem_euclid(2);
    even.max(2)
}

fn fit_within_source(width: i32, height: i32, source_width: i32, source_height: i32) -> (i32, i32) {
    if width <= source_width && height <= source_height {
        return (width, height);
    }
    let scale = (f64::from(source_width) / f64::from(width))
        .min(f64::from(source_height) / f64::from(height))
        .min(1.0);
    (
        even_floor(f64::from(width) * scale),
        even_floor(f64::from(height) * scale),
    )
}

fn target_dimensions_for_source(
    preset_width: i32,
    preset_height: i32,
    source_dimensions: Option<(i32, i32)>,
) -> (i32, i32) {
    let short_edge = preset_width.min(preset_height);
    let Some((source_width, source_height)) = source_dimensions else {
        return (preset_width, preset_height);
    };
    if source_height > source_width {
        let width = short_edge;
        let height =
            even_floor(f64::from(short_edge) * f64::from(source_height) / f64::from(source_width));
        fit_within_source(width, height, source_width, source_height)
    } else if source_width > source_height {
        let width =
            even_floor(f64::from(short_edge) * f64::from(source_width) / f64::from(source_height));
        let height = short_edge;
        fit_within_source(width, height, source_width, source_height)
    } else {
        fit_within_source(short_edge, short_edge, source_width, source_height)
    }
}

fn target_video_bitrate_kbps(
    base_video_kbps: i32,
    preset_width: i32,
    preset_height: i32,
    width: i32,
    height: i32,
) -> i32 {
    let base_pixels = i64::from(preset_width) * i64::from(preset_height);
    if base_pixels <= 0 {
        return base_video_kbps;
    }
    let target_pixels = i64::from(width) * i64::from(height);
    let scaled = (f64::from(base_video_kbps) * target_pixels as f64 / base_pixels as f64).round();
    even_floor(scaled.max(64.0))
}

fn estimate_transcoded_bytes(seconds: f64, video_kbps: i32, audio_kbps: i32, overhead: f64) -> i64 {
    if seconds <= 0.0 {
        return 0;
    }
    let bitrate_bps = f64::from(video_kbps + audio_kbps) * 1000.0;
    let media_bytes = seconds * bitrate_bps / 8.0;
    (media_bytes * overhead).ceil().max(1.0) as i64
}

fn ceil_ratio(value: i64, numerator: i64, denominator: i64) -> i64 {
    if denominator <= 0 {
        return 0;
    }
    ((i128::from(value) * i128::from(numerator) + i128::from(denominator) - 1)
        / i128::from(denominator)) as i64
}

fn ceil_ratio_u128(value: u128, numerator: i64, denominator: i64) -> u128 {
    if value == 0 || numerator <= 0 || denominator <= 0 {
        return 0;
    }
    let numerator = numerator as u128;
    let denominator = denominator as u128;
    (value * numerator).div_ceil(denominator)
}

fn quote_sample_bytes(byte_size: i64, max_sample_bytes: usize) -> Option<i64> {
    if byte_size <= 0 {
        return None;
    }
    let max_sample_bytes = max_sample_bytes.max(MIN_ANTD_SELF_ENCRYPTION_BYTES) as i64;
    Some(
        byte_size
            .min(max_sample_bytes)
            .max(MIN_ANTD_SELF_ENCRYPTION_BYTES as i64),
    )
}

#[derive(Clone)]
struct QuoteValue {
    sampled: bool,
    storage_cost_atto: u128,
    estimated_gas_cost_wei: u128,
    chunk_count: i64,
    payment_mode: String,
}

async fn quote_data_size(
    state: &AppState,
    byte_size: i64,
    cache: &mut std::collections::HashMap<i64, QuoteValue>,
) -> Result<QuoteValue, ApiError> {
    if byte_size <= 0 {
        return Ok(QuoteValue {
            sampled: false,
            storage_cost_atto: 0,
            estimated_gas_cost_wei: 0,
            chunk_count: 0,
            payment_mode: state.config.antd_payment_mode.clone(),
        });
    }

    let quote_bytes = byte_size.max(MIN_ANTD_SELF_ENCRYPTION_BYTES as i64);
    let sample_bytes = quote_sample_bytes(byte_size, state.config.upload_quote_max_sample_bytes)
        .unwrap_or(MIN_ANTD_SELF_ENCRYPTION_BYTES as i64);
    if cache.get(&sample_bytes).is_none() {
        let estimate = state
            .antd
            .data_cost_for_size(sample_bytes as usize)
            .await
            .map_err(|err| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("Could not get Autonomi price quote: {err}"),
                )
            })?;
        cache.insert(
            sample_bytes,
            QuoteValue {
                sampled: false,
                storage_cost_atto: parse_cost_u128(estimate.cost.as_deref()),
                estimated_gas_cost_wei: parse_cost_u128(estimate.estimated_gas_cost_wei.as_deref()),
                chunk_count: estimate.chunk_count.unwrap_or(0),
                payment_mode: estimate
                    .payment_mode
                    .unwrap_or_else(|| state.config.antd_payment_mode.clone()),
            },
        );
    }

    let quoted = cache.get(&sample_bytes).cloned().unwrap();
    if sample_bytes == quote_bytes {
        return Ok(quoted);
    }

    Ok(QuoteValue {
        sampled: true,
        storage_cost_atto: ceil_ratio_u128(quoted.storage_cost_atto, quote_bytes, sample_bytes),
        estimated_gas_cost_wei: ceil_ratio_u128(
            quoted.estimated_gas_cost_wei,
            quote_bytes,
            sample_bytes,
        ),
        chunk_count: ceil_ratio(quoted.chunk_count, quote_bytes, sample_bytes).max(1),
        payment_mode: quoted.payment_mode,
    })
}

fn parse_cost_u128(value: Option<&str>) -> u128 {
    value
        .and_then(|value| value.parse::<u128>().ok())
        .unwrap_or(0)
}

async fn build_upload_quote(
    state: &AppState,
    request: UploadQuoteRequest,
) -> Result<UploadQuoteOut, ApiError> {
    if request.duration_seconds <= 0.0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "duration_seconds must be greater than zero",
        ));
    }

    let source_dimensions = match (request.source_width, request.source_height) {
        (None, None) => None,
        (Some(width), Some(height)) if width > 0 && height > 0 => Some((width, height)),
        (Some(_), Some(_)) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "source_width and source_height must be greater than zero",
            ))
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "source_width and source_height must be provided together",
            ))
        }
    };

    if let Some((width, height)) = source_dimensions {
        enforce_upload_media_limits(state, request.duration_seconds, width, height)?;
    } else if request.duration_seconds > state.config.upload_max_duration_seconds {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Video duration exceeds upload limit",
        ));
    }
    if request.upload_original {
        match request.source_size_bytes {
            Some(size) if size > 0 => {}
            Some(_) => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "source_size_bytes must be greater than zero when upload_original is true",
                ))
            }
            None => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "source_size_bytes must be provided when upload_original is true",
                ))
            }
        }
    }

    let selected: Vec<_> = request
        .resolutions
        .iter()
        .filter_map(|resolution| {
            resolution_preset(resolution).map(|preset| (resolution.clone(), preset))
        })
        .collect();
    if selected.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            supported_resolutions_error(),
        ));
    }

    let mut quote_cache = std::collections::HashMap::new();
    let mut variants = Vec::new();
    let mut total_storage_cost = 0_u128;
    let mut total_gas_cost = 0_u128;
    let mut total_bytes = 0_i64;
    let mut total_segments = 0_i64;
    let mut any_sampled = false;
    let mut original_file = None;

    for (resolution, (preset_width, preset_height, video_kbps, audio_kbps)) in selected {
        let (width, height) =
            target_dimensions_for_source(preset_width, preset_height, source_dimensions);
        let video_kbps =
            target_video_bitrate_kbps(video_kbps, preset_width, preset_height, width, height);
        let full_segments =
            (request.duration_seconds / state.config.hls_segment_duration).floor() as i64;
        let mut remainder =
            request.duration_seconds - (full_segments as f64 * state.config.hls_segment_duration);
        if remainder < 0.01 {
            remainder = 0.0;
        }
        let segment_count = full_segments + if remainder > 0.0 { 1 } else { 0 };
        let full_segment_bytes = estimate_transcoded_bytes(
            state
                .config
                .hls_segment_duration
                .min(request.duration_seconds),
            video_kbps,
            audio_kbps,
            state.config.upload_quote_transcoded_overhead,
        );
        let full_quote = quote_data_size(state, full_segment_bytes, &mut quote_cache).await?;

        let mut variant_storage_cost = full_quote.storage_cost_atto * full_segments as u128;
        let mut variant_gas_cost = full_quote.estimated_gas_cost_wei * full_segments as u128;
        let mut variant_bytes = full_segment_bytes * full_segments;
        let mut variant_chunks = full_quote.chunk_count * full_segments;
        any_sampled = any_sampled || full_quote.sampled;

        if remainder > 0.0 {
            let final_segment_bytes = estimate_transcoded_bytes(
                remainder,
                video_kbps,
                audio_kbps,
                state.config.upload_quote_transcoded_overhead,
            );
            let final_quote = quote_data_size(state, final_segment_bytes, &mut quote_cache).await?;
            variant_storage_cost += final_quote.storage_cost_atto;
            variant_gas_cost += final_quote.estimated_gas_cost_wei;
            variant_bytes += final_segment_bytes;
            variant_chunks += final_quote.chunk_count;
            any_sampled = any_sampled || final_quote.sampled;
        }

        variants.push(UploadQuoteVariantOut {
            resolution,
            width,
            height,
            segment_count,
            estimated_bytes: variant_bytes,
            chunk_count: variant_chunks,
            storage_cost_atto: variant_storage_cost.to_string(),
            estimated_gas_cost_wei: variant_gas_cost.to_string(),
            payment_mode: full_quote.payment_mode,
        });
        total_storage_cost += variant_storage_cost;
        total_gas_cost += variant_gas_cost;
        total_bytes += variant_bytes;
        total_segments += segment_count;
    }

    if request.upload_original {
        let source_size_bytes = request.source_size_bytes.unwrap_or_default();
        let quote = quote_data_size(state, source_size_bytes, &mut quote_cache).await?;
        total_storage_cost += quote.storage_cost_atto;
        total_gas_cost += quote.estimated_gas_cost_wei;
        total_bytes += source_size_bytes;
        any_sampled = any_sampled || quote.sampled;
        original_file = Some(UploadQuoteOriginalOut {
            estimated_bytes: source_size_bytes,
            chunk_count: quote.chunk_count,
            storage_cost_atto: quote.storage_cost_atto.to_string(),
            estimated_gas_cost_wei: quote.estimated_gas_cost_wei.to_string(),
            payment_mode: quote.payment_mode,
        });
    }

    let manifest_bytes = 4096 + (variants.len() as i64 * 1024) + (total_segments * 220);
    let catalog_bytes = 2048 + (variants.len() as i64 * 512);
    let metadata_quote =
        quote_data_size(state, manifest_bytes + catalog_bytes, &mut quote_cache).await?;
    total_storage_cost += metadata_quote.storage_cost_atto;
    total_gas_cost += metadata_quote.estimated_gas_cost_wei;
    total_bytes += manifest_bytes + catalog_bytes;
    any_sampled = any_sampled || metadata_quote.sampled;

    Ok(UploadQuoteOut {
        duration_seconds: request.duration_seconds,
        segment_duration: state.config.hls_segment_duration,
        payment_mode: state.config.antd_payment_mode.clone(),
        estimated_bytes: total_bytes,
        segment_count: total_segments,
        storage_cost_atto: total_storage_cost.to_string(),
        estimated_gas_cost_wei: total_gas_cost.to_string(),
        metadata_bytes: manifest_bytes + catalog_bytes,
        sampled: any_sampled,
        original_file,
        variants,
    })
}

fn enforce_upload_media_limits(
    state: &AppState,
    duration_seconds: f64,
    width: i32,
    height: i32,
) -> Result<(), ApiError> {
    if duration_seconds > state.config.upload_max_duration_seconds {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Video duration exceeds upload limit",
        ));
    }
    let pixel_count = i64::from(width) * i64::from(height);
    let long_edge = i64::from(width.max(height));
    if long_edge > state.config.upload_max_source_long_edge
        || pixel_count > state.config.upload_max_source_pixels
    {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Video resolution exceeds upload limit",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_dimensions_follow_source_orientation() {
        assert_eq!(
            target_dimensions_for_source(1920, 1080, Some((1080, 1920))),
            (1080, 1920)
        );
        assert_eq!(
            target_dimensions_for_source(1920, 1080, Some((1920, 1080))),
            (1920, 1080)
        );
        assert_eq!(
            target_dimensions_for_source(1920, 1080, Some((1600, 1200))),
            (1440, 1080)
        );
        assert_eq!(
            target_dimensions_for_source(1920, 1080, Some((1080, 1080))),
            (1080, 1080)
        );
        assert_eq!(
            target_dimensions_for_source(2560, 1440, Some((3440, 1440))),
            (3440, 1440)
        );
    }

    #[test]
    fn estimate_transcoded_bytes_uses_bitrate_and_overhead() {
        assert_eq!(estimate_transcoded_bytes(1.0, 500, 96, 1.08), 80460);
    }

    #[test]
    fn parses_quote_costs_above_i64_max() {
        let ten_ant_atto = "10000000000000000000";
        assert_eq!(
            parse_cost_u128(Some(ten_ant_atto)),
            10_000_000_000_000_000_000_u128
        );
        assert_eq!(parse_cost_u128(Some("-1")), 0);
        assert_eq!(parse_cost_u128(Some("not-a-number")), 0);
    }

    #[test]
    fn scales_sampled_quote_costs_without_signed_overflow() {
        let value = 10_000_000_000_000_000_000_u128;
        assert_eq!(
            ceil_ratio_u128(value, 3, 2),
            15_000_000_000_000_000_000_u128
        );
    }

    #[test]
    fn quote_sample_bytes_respects_autonomi_minimum() {
        assert_eq!(quote_sample_bytes(0, 16), None);
        assert_eq!(quote_sample_bytes(1, 16), Some(3));
        assert_eq!(quote_sample_bytes(2, 16), Some(3));
        assert_eq!(quote_sample_bytes(10, 16), Some(10));
        assert_eq!(quote_sample_bytes(100, 16), Some(16));
        assert_eq!(quote_sample_bytes(100, 1), Some(3));
    }

    #[test]
    fn treats_stream_abort_on_file_upload_route_as_missing_endpoint() {
        let err = anyhow::anyhow!(
            "error sending request for url (http://antd:8082/v1/file/public?payment_mode=auto&verify=true)"
        );
        assert!(is_missing_file_upload_endpoint(&err));

        let err = anyhow::anyhow!("error sending request for url (http://antd:8082/health)");
        assert!(!is_missing_file_upload_endpoint(&err));
    }

    #[test]
    fn parses_durable_job_kinds() {
        assert_eq!(
            JobKind::parse(JOB_KIND_PROCESS_VIDEO),
            Some(JobKind::ProcessVideo)
        );
        assert_eq!(
            JobKind::parse(JOB_KIND_UPLOAD_VIDEO),
            Some(JobKind::UploadVideo)
        );
        assert_eq!(
            JobKind::parse(JOB_KIND_PUBLISH_CATALOG),
            Some(JobKind::PublishCatalog)
        );
        assert_eq!(JobKind::parse("unknown"), None);
    }

    #[test]
    fn durable_job_retry_backoff_caps() {
        assert_eq!(job_retry_delay_seconds(1), 30);
        assert_eq!(job_retry_delay_seconds(2), 60);
        assert_eq!(job_retry_delay_seconds(6), 900);
        assert_eq!(job_retry_delay_seconds(20), 900);
    }

    #[test]
    fn normalizes_cors_origin_without_paths_or_wildcards() {
        assert_eq!(
            normalize_cors_origin("http://localhost:5173/").unwrap(),
            "http://localhost:5173"
        );
        assert!(normalize_cors_origin("*").is_err());
        assert!(normalize_cors_origin("http://localhost/app").is_err());
    }

    #[test]
    fn sanitizes_upload_filename_like_admin_service() {
        assert_eq!(
            sanitize_upload_filename(Some("../My Video!!.MP4")),
            "My_Video.mp4"
        );
        assert_eq!(sanitize_upload_filename(Some("...")), "upload");
    }

    #[test]
    fn parses_only_supported_resolutions() {
        assert_eq!(
            parse_resolutions("720p, nope,1440p,1080p,4k"),
            vec!["720p", "1440p", "1080p", "4k"]
        );
    }
}
