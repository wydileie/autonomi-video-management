use std::{
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::Duration as StdDuration,
};

use axum::{
    extract::{Path, State},
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
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use uuid::Uuid;

const STATUS_READY: &str = "ready";
const DEFAULT_API_PORT: u16 = 8000;
const CATALOG_CONTENT_TYPE: &str = "application/vnd.autonomi.video.catalog+json;v=1";

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    pool: PgPool,
    antd: AntdRestClient,
}

#[derive(Clone)]
struct Config {
    db_dsn: String,
    antd_url: String,
    antd_payment_mode: String,
    admin_username: String,
    admin_password: String,
    admin_auth_secret: String,
    admin_auth_ttl_hours: i64,
    catalog_state_path: PathBuf,
    catalog_bootstrap_address: Option<String>,
    cors_allowed_origins: Vec<HeaderValue>,
    bind_addr: SocketAddr,
    hls_segment_duration: f64,
    upload_max_duration_seconds: f64,
    upload_max_source_pixels: i64,
    upload_max_source_long_edge: i64,
    upload_quote_transcoded_overhead: f64,
    upload_quote_max_sample_bytes: usize,
    final_quote_approval_ttl_seconds: i64,
    antd_approve_on_startup: bool,
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

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    autonomi: AutonomiHealth,
    payment_mode: String,
    final_quote_approval_ttl_seconds: i64,
    implementation: &'static str,
    parity: &'static str,
}

#[derive(Serialize)]
struct AutonomiHealth {
    ok: bool,
    network: Option<String>,
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
    show_original_filename: bool,
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
    error_message: Option<String>,
    final_quote: Option<Value>,
    final_quote_created_at: Option<String>,
    approval_expires_at: Option<String>,
    variants: Vec<VariantOut>,
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
    variants: Vec<UploadQuoteVariantOut>,
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

    fn not_implemented(workflow: &str) -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            format!(
                "rust_admin does not yet implement {workflow}; use python_admin for this workflow during migration"
            ),
        )
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

    let antd = AntdRestClient::new(&config.antd_url)?;
    ensure_autonomi_ready(&config, &antd).await?;

    let state = AppState {
        config: config.clone(),
        pool,
        antd,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(login))
        .route("/auth/me", get(auth_me))
        .route("/catalog", get(get_catalog))
        .route("/videos/upload/quote", post(quote_video_upload))
        .route("/videos/upload", post(upload_video_not_migrated))
        .route("/videos", get(list_videos))
        .route("/admin/videos", get(admin_list_videos))
        .route("/videos/:video_id", get(get_video).delete(delete_video_not_migrated))
        .route("/admin/videos/:video_id", get(admin_get_video).delete(delete_video_not_migrated))
        .route("/videos/:video_id/status", get(video_status))
        .route("/videos/:video_id/approve", post(approve_video_not_migrated))
        .route("/admin/videos/:video_id/approve", post(approve_video_not_migrated))
        .route("/admin/videos/:video_id/visibility", patch(update_video_visibility))
        .route("/admin/videos/:video_id/publication", patch(update_publication_not_migrated))
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

        let antd_payment_mode = env::var("ANTD_PAYMENT_MODE").unwrap_or_else(|_| "auto".into());
        if !matches!(antd_payment_mode.as_str(), "auto" | "merkle" | "single") {
            anyhow::bail!("ANTD_PAYMENT_MODE must be one of auto, merkle, single");
        }

        let hls_segment_duration = parse_f64_env("HLS_SEGMENT_DURATION", 1.0)?;
        if hls_segment_duration <= 0.0 {
            anyhow::bail!("HLS_SEGMENT_DURATION must be greater than zero");
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

        Ok(Self {
            db_dsn,
            antd_url: env::var("ANTD_URL").unwrap_or_else(|_| "http://localhost:8082".into()),
            antd_payment_mode,
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
            hls_segment_duration,
            upload_max_duration_seconds: parse_f64_env("UPLOAD_MAX_DURATION_SECONDS", 4.0 * 60.0 * 60.0)?,
            upload_max_source_pixels: parse_i64_env("UPLOAD_MAX_SOURCE_PIXELS", 7680 * 4320)?,
            upload_max_source_long_edge: parse_i64_env("UPLOAD_MAX_SOURCE_LONG_EDGE", 7680)?,
            upload_quote_transcoded_overhead,
            upload_quote_max_sample_bytes,
            final_quote_approval_ttl_seconds: parse_i64_env(
                "FINAL_QUOTE_APPROVAL_TTL_SECONDS",
                4 * 60 * 60,
            )?,
            antd_approve_on_startup: parse_bool_env("ANTD_APPROVE_ON_STARTUP", true),
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
        .map(|value| !matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(default_value)
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
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE, Method::OPTIONS])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::RANGE,
        ]))
}

impl AntdRestClient {
    fn new(base_url: &str) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .timeout(StdDuration::from_secs(60))
                .build()?,
        })
    }

    async fn health(&self) -> anyhow::Result<AntdHealthResponse> {
        self.request_json(reqwest::Method::GET, "/health", Option::<Value>::None)
            .await
    }

    async fn wallet_address(&self) -> anyhow::Result<Value> {
        self.request_json(reqwest::Method::GET, "/v1/wallet/address", Option::<Value>::None)
            .await
    }

    async fn wallet_balance(&self) -> anyhow::Result<Value> {
        self.request_json(reqwest::Method::GET, "/v1/wallet/balance", Option::<Value>::None)
            .await
    }

    async fn wallet_approve(&self) -> anyhow::Result<Value> {
        self.request_json(reqwest::Method::POST, "/v1/wallet/approve", Option::<Value>::None)
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
    Ok(())
}

async fn ensure_schema(pool: &PgPool) -> anyhow::Result<()> {
    const SCHEMA_SQL: &str = r#"
        CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

        CREATE TABLE IF NOT EXISTS videos (
            id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
            title TEXT NOT NULL,
            original_filename TEXT NOT NULL,
            description TEXT,
            status TEXT NOT NULL DEFAULT 'pending',
            manifest_address TEXT,
            catalog_address TEXT,
            error_message TEXT,
            job_dir TEXT,
            job_source_path TEXT,
            requested_resolutions JSONB,
            final_quote JSONB,
            final_quote_created_at TIMESTAMPTZ,
            approval_expires_at TIMESTAMPTZ,
            is_public BOOLEAN NOT NULL DEFAULT FALSE,
            show_original_filename BOOLEAN NOT NULL DEFAULT FALSE,
            show_manifest_address BOOLEAN NOT NULL DEFAULT FALSE,
            created_at TIMESTAMPTZ DEFAULT NOW(),
            updated_at TIMESTAMPTZ DEFAULT NOW(),
            user_id TEXT
        );

        ALTER TABLE videos
            ADD COLUMN IF NOT EXISTS manifest_address TEXT,
            ADD COLUMN IF NOT EXISTS catalog_address TEXT,
            ADD COLUMN IF NOT EXISTS error_message TEXT,
            ADD COLUMN IF NOT EXISTS job_dir TEXT,
            ADD COLUMN IF NOT EXISTS job_source_path TEXT,
            ADD COLUMN IF NOT EXISTS requested_resolutions JSONB,
            ADD COLUMN IF NOT EXISTS final_quote JSONB,
            ADD COLUMN IF NOT EXISTS final_quote_created_at TIMESTAMPTZ,
            ADD COLUMN IF NOT EXISTS approval_expires_at TIMESTAMPTZ,
            ADD COLUMN IF NOT EXISTS is_public BOOLEAN NOT NULL DEFAULT FALSE,
            ADD COLUMN IF NOT EXISTS show_original_filename BOOLEAN NOT NULL DEFAULT FALSE,
            ADD COLUMN IF NOT EXISTS show_manifest_address BOOLEAN NOT NULL DEFAULT FALSE;

        CREATE TABLE IF NOT EXISTS video_variants (
            id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
            video_id UUID NOT NULL REFERENCES videos(id) ON DELETE CASCADE,
            resolution TEXT NOT NULL,
            width INTEGER NOT NULL,
            height INTEGER NOT NULL,
            video_bitrate INTEGER NOT NULL,
            audio_bitrate INTEGER NOT NULL,
            segment_duration FLOAT NOT NULL DEFAULT 10.0,
            total_duration FLOAT,
            segment_count INTEGER,
            created_at TIMESTAMPTZ DEFAULT NOW()
        );

        CREATE TABLE IF NOT EXISTS video_segments (
            id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
            variant_id UUID NOT NULL REFERENCES video_variants(id) ON DELETE CASCADE,
            segment_index INTEGER NOT NULL,
            autonomi_address TEXT,
            autonomi_cost_atto TEXT,
            autonomi_payment_mode TEXT,
            duration FLOAT NOT NULL DEFAULT 10.0,
            byte_size BIGINT,
            local_path TEXT,
            created_at TIMESTAMPTZ DEFAULT NOW(),
            UNIQUE (variant_id, segment_index)
        );

        ALTER TABLE video_segments
            ADD COLUMN IF NOT EXISTS autonomi_cost_atto TEXT,
            ADD COLUMN IF NOT EXISTS autonomi_payment_mode TEXT,
            ADD COLUMN IF NOT EXISTS local_path TEXT;

        ALTER TABLE video_segments
            ALTER COLUMN autonomi_address DROP NOT NULL;

        CREATE INDEX IF NOT EXISTS idx_videos_status ON videos(status);
        CREATE INDEX IF NOT EXISTS idx_videos_is_public ON videos(is_public);
        CREATE INDEX IF NOT EXISTS idx_variants_video ON video_variants(video_id);
        CREATE INDEX IF NOT EXISTS idx_segments_variant ON video_segments(variant_id);
    "#;

    for statement in SCHEMA_SQL.split(';').map(str::trim).filter(|sql| !sql.is_empty()) {
        sqlx::query(statement).execute(pool).await?;
    }
    Ok(())
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    match state.antd.health().await {
        Ok(status) => Json(HealthResponse {
            ok: status.status.eq_ignore_ascii_case("ok"),
            autonomi: AutonomiHealth {
                ok: status.status.eq_ignore_ascii_case("ok"),
                network: status.network,
                error: None,
            },
            payment_mode: state.config.antd_payment_mode.clone(),
            final_quote_approval_ttl_seconds: state.config.final_quote_approval_ttl_seconds,
            implementation: "rust_admin",
            parity: "migration",
        })
        .into_response(),
        Err(err) => Json(HealthResponse {
            ok: false,
            autonomi: AutonomiHealth {
                ok: false,
                network: None,
                error: Some(err.to_string()),
            },
            payment_mode: state.config.antd_payment_mode.clone(),
            final_quote_approval_ttl_seconds: state.config.final_quote_approval_ttl_seconds,
            implementation: "rust_admin",
            parity: "migration",
        })
        .into_response(),
    }
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
        .filter(|entry| entry.get("status").and_then(Value::as_str).unwrap_or(STATUS_READY) == STATUS_READY)
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
               is_public, show_original_filename, show_manifest_address
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
    let manifest_address = catalog
        .get("videos")
        .and_then(Value::as_array)
        .and_then(|videos| {
            videos
                .iter()
                .find(|entry| entry.get("id").and_then(Value::as_str) == Some(video_id.as_str()))
        })
        .and_then(|entry| entry.get("manifest_address").and_then(Value::as_str))
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;

    let manifest = load_json_from_autonomi(&state, manifest_address).await?;
    Ok(Json(manifest_to_video_out(&state, &manifest, Some(manifest_address), true)))
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
    let row = sqlx::query(
        r#"
        SELECT status, manifest_address, catalog_address, error_message,
               show_manifest_address
        FROM videos WHERE id=$1
        "#,
    )
    .bind(&video_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    if let Some(row) = row {
        let show_manifest_address = row.try_get::<bool, _>("show_manifest_address").unwrap_or(false);
        let manifest_address = if show_manifest_address {
            row.try_get::<Option<String>, _>("manifest_address").ok().flatten()
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
    let (manifest, manifest_address) = loaded
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
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
    .bind(request.show_original_filename)
    .bind(request.show_manifest_address)
    .bind(&video_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(db_error)?;

    let row = row.ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "Video not found"))?;
    let status: String = row.try_get("status").unwrap_or_default();
    let is_public: bool = row.try_get("is_public").unwrap_or(false);
    if status == STATUS_READY && is_public {
        return Err(ApiError::not_implemented(
            "catalog republish after visibility update",
        ));
    }

    Ok(Json(get_db_video(&state, &video_id, true).await?))
}

async fn upload_video_not_migrated() -> Result<Response, ApiError> {
    Err(ApiError::not_implemented("multipart upload and FFmpeg transcoding"))
}

async fn approve_video_not_migrated() -> Result<Response, ApiError> {
    Err(ApiError::not_implemented("approved Autonomi segment upload"))
}

async fn update_publication_not_migrated(
    Json(request): Json<VideoPublicationUpdate>,
) -> Result<Response, ApiError> {
    let _ = request.is_public;
    Err(ApiError::not_implemented("catalog publication changes"))
}

async fn delete_video_not_migrated() -> Result<Response, ApiError> {
    Err(ApiError::not_implemented("video delete and catalog removal"))
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

fn read_catalog_address(config: &Config) -> Option<String> {
    if let Ok(raw) = fs::read_to_string(&config.catalog_state_path) {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            if let Some(address) = value
                .get("catalog_address")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|address| !address.is_empty())
            {
                return Some(address.to_string());
            }
        }
    }
    config.catalog_bootstrap_address.clone()
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
    serde_json::from_slice(&data)
        .map_err(|err| ApiError::new(StatusCode::BAD_GATEWAY, format!("invalid JSON from Autonomi: {err}")))
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
    let row = sqlx::query(
        r#"
        SELECT id, title, original_filename, description, status, created_at,
               manifest_address, catalog_address, error_message, final_quote,
               final_quote_created_at, approval_expires_at,
               is_public, show_original_filename, show_manifest_address
        FROM videos WHERE id=$1
        "#,
    )
    .bind(video_id)
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
        created_at: created_at.to_string(),
        manifest_address: row.try_get("manifest_address").ok().flatten(),
        catalog_address,
        is_public: row.try_get("is_public").unwrap_or(false),
        show_original_filename: row.try_get("show_original_filename").unwrap_or(false),
        show_manifest_address: row.try_get("show_manifest_address").unwrap_or(false),
        error_message: row.try_get("error_message").ok().flatten(),
        final_quote: row.try_get("final_quote").ok().flatten(),
        final_quote_created_at: final_quote_created_at.map(|value| value.to_string()),
        approval_expires_at: approval_expires_at.map(|value| value.to_string()),
        variants,
    })
}

fn catalog_entry_to_video_out(entry: &Value, catalog_address: Option<&str>) -> VideoOut {
    let show_original_filename = entry
        .get("show_original_filename")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let show_manifest_address = entry
        .get("show_manifest_address")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    VideoOut {
        id: string_field(entry, "id"),
        title: string_field(entry, "title"),
        original_filename: if show_original_filename {
            opt_string_field(entry, "original_filename")
        } else {
            None
        },
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
        show_original_filename,
        show_manifest_address,
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
                id: format!("{}:{}", string_field(entry, "id"), string_field(variant, "resolution")),
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
    let video_id = string_field(manifest, "id");
    VideoOut {
        id: video_id.clone(),
        title: string_field(manifest, "title"),
        original_filename: if !public || show_original_filename {
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
        show_original_filename,
        show_manifest_address,
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
                            duration: segment.get("duration").and_then(Value::as_f64).unwrap_or(0.0),
                        })
                        .collect()
                },
            })
            .collect(),
    }
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

fn resolution_preset(resolution: &str) -> Option<(i32, i32, i32, i32)> {
    match resolution {
        "8k" => Some((7680, 4320, 45000, 320)),
        "4k" => Some((3840, 2160, 16000, 256)),
        "360p" => Some((640, 360, 500, 96)),
        "480p" => Some((854, 480, 1000, 128)),
        "720p" => Some((1280, 720, 2500, 128)),
        "1080p" => Some((1920, 1080, 5000, 192)),
        _ => None,
    }
}

fn target_dimensions_for_source(
    preset_width: i32,
    preset_height: i32,
    source_dimensions: Option<(i32, i32)>,
) -> (i32, i32) {
    let long_edge = preset_width.max(preset_height);
    let short_edge = preset_width.min(preset_height);
    let Some((source_width, source_height)) = source_dimensions else {
        return (preset_width, preset_height);
    };
    if source_height > source_width {
        (short_edge, long_edge)
    } else if source_width > source_height {
        (long_edge, short_edge)
    } else {
        (preset_width, preset_height)
    }
}

fn estimate_transcoded_bytes(
    seconds: f64,
    video_kbps: i32,
    audio_kbps: i32,
    overhead: f64,
) -> i64 {
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

#[derive(Clone)]
struct QuoteValue {
    sampled: bool,
    storage_cost_atto: i64,
    estimated_gas_cost_wei: i64,
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

    let sample_bytes = byte_size.min(state.config.upload_quote_max_sample_bytes as i64);
    if cache.get(&sample_bytes).is_none() {
        let mut data = vec![0_u8; sample_bytes as usize];
        rand::thread_rng().fill_bytes(&mut data);
        let estimate = state
            .antd
            .data_cost(&data)
            .await
            .map_err(|err| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, format!("Could not get Autonomi price quote: {err}")))?;
        cache.insert(
            sample_bytes,
            QuoteValue {
                sampled: false,
                storage_cost_atto: parse_cost_i64(estimate.cost.as_deref()),
                estimated_gas_cost_wei: parse_cost_i64(estimate.estimated_gas_cost_wei.as_deref()),
                chunk_count: estimate.chunk_count.unwrap_or(0),
                payment_mode: estimate
                    .payment_mode
                    .unwrap_or_else(|| state.config.antd_payment_mode.clone()),
            },
        );
    }

    let quoted = cache.get(&sample_bytes).cloned().unwrap();
    if sample_bytes == byte_size {
        return Ok(quoted);
    }

    Ok(QuoteValue {
        sampled: true,
        storage_cost_atto: ceil_ratio(quoted.storage_cost_atto, byte_size, sample_bytes),
        estimated_gas_cost_wei: ceil_ratio(quoted.estimated_gas_cost_wei, byte_size, sample_bytes),
        chunk_count: ceil_ratio(quoted.chunk_count, byte_size, sample_bytes).max(1),
        payment_mode: quoted.payment_mode,
    })
}

fn parse_cost_i64(value: Option<&str>) -> i64 {
    value.and_then(|value| value.parse::<i64>().ok()).unwrap_or(0)
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

    let selected: Vec<_> = request
        .resolutions
        .iter()
        .filter_map(|resolution| resolution_preset(resolution).map(|preset| (resolution.clone(), preset)))
        .collect();
    if selected.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "No valid resolutions. Choose from: ['8k', '4k', '360p', '480p', '720p', '1080p']",
        ));
    }

    let mut quote_cache = std::collections::HashMap::new();
    let mut variants = Vec::new();
    let mut total_storage_cost = 0_i64;
    let mut total_gas_cost = 0_i64;
    let mut total_bytes = 0_i64;
    let mut total_segments = 0_i64;
    let mut any_sampled = false;

    for (resolution, (preset_width, preset_height, video_kbps, audio_kbps)) in selected {
        let (width, height) =
            target_dimensions_for_source(preset_width, preset_height, source_dimensions);
        let full_segments =
            (request.duration_seconds / state.config.hls_segment_duration).floor() as i64;
        let mut remainder =
            request.duration_seconds - (full_segments as f64 * state.config.hls_segment_duration);
        if remainder < 0.01 {
            remainder = 0.0;
        }
        let segment_count = full_segments + if remainder > 0.0 { 1 } else { 0 };
        let full_segment_bytes = estimate_transcoded_bytes(
            state.config.hls_segment_duration.min(request.duration_seconds),
            video_kbps,
            audio_kbps,
            state.config.upload_quote_transcoded_overhead,
        );
        let full_quote = quote_data_size(state, full_segment_bytes, &mut quote_cache).await?;

        let mut variant_storage_cost = full_quote.storage_cost_atto * full_segments;
        let mut variant_gas_cost = full_quote.estimated_gas_cost_wei * full_segments;
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

    let manifest_bytes = 4096 + (variants.len() as i64 * 1024) + (total_segments * 220);
    let catalog_bytes = 2048 + (variants.len() as i64 * 512);
    let metadata_quote = quote_data_size(state, manifest_bytes + catalog_bytes, &mut quote_cache).await?;
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
    }

    #[test]
    fn estimate_transcoded_bytes_uses_bitrate_and_overhead() {
        assert_eq!(estimate_transcoded_bytes(1.0, 500, 96, 1.08), 80460);
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
}
