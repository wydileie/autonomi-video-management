use std::{env, net::SocketAddr, path::PathBuf, time::Duration as StdDuration};

use axum::http::{header, HeaderName, HeaderValue, Method};
use subtle::ConstantTimeEq;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::{
    DEFAULT_ADMIN_REFRESH_TOKEN_TTL_HOURS, DEFAULT_API_PORT, MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthCookieSameSite {
    Strict,
    Lax,
    None,
}

impl AuthCookieSameSite {
    pub(crate) fn as_cookie_value(self) -> &'static str {
        match self {
            Self::Strict => "Strict",
            Self::Lax => "Lax",
            Self::None => "None",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(Self::Strict),
            "lax" => Some(Self::Lax),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) db_dsn: String,
    pub(crate) antd_url: String,
    pub(crate) antd_internal_token: Option<String>,
    pub(crate) antd_payment_mode: String,
    pub(crate) antd_metadata_payment_mode: String,
    pub(crate) admin_username: String,
    pub(crate) admin_password: String,
    pub(crate) admin_auth_secret: String,
    pub(crate) admin_auth_ttl_hours: i64,
    pub(crate) admin_auth_cookie_secure: bool,
    pub(crate) catalog_state_path: PathBuf,
    pub(crate) catalog_bootstrap_address: Option<String>,
    pub(crate) cors_allowed_origins: Vec<HeaderValue>,
    pub(crate) bind_addr: SocketAddr,
    pub(crate) admin_request_timeout_seconds: f64,
    pub(crate) admin_upload_request_timeout_seconds: f64,
    pub(crate) upload_temp_dir: PathBuf,
    pub(crate) upload_max_file_bytes: u64,
    pub(crate) upload_min_free_bytes: u64,
    pub(crate) upload_max_concurrent_saves: usize,
    pub(crate) upload_ffprobe_timeout_seconds: f64,
    pub(crate) hls_segment_duration: f64,
    pub(crate) ffmpeg_threads: usize,
    pub(crate) ffmpeg_filter_threads: usize,
    pub(crate) ffmpeg_max_parallel_renditions: usize,
    pub(crate) upload_max_duration_seconds: f64,
    pub(crate) upload_max_source_pixels: i64,
    pub(crate) upload_max_source_long_edge: i64,
    pub(crate) upload_quote_transcoded_overhead: f64,
    pub(crate) upload_quote_max_sample_bytes: usize,
    pub(crate) final_quote_approval_ttl_seconds: i64,
    pub(crate) approval_cleanup_interval_seconds: u64,
    pub(crate) antd_upload_verify: bool,
    pub(crate) antd_upload_retries: usize,
    pub(crate) antd_upload_timeout_seconds: f64,
    pub(crate) antd_quote_concurrency: usize,
    pub(crate) antd_upload_concurrency: usize,
    pub(crate) antd_approve_on_startup: bool,
    pub(crate) antd_require_cost_ready: bool,
    pub(crate) antd_direct_upload_max_bytes: usize,
    pub(crate) admin_job_workers: usize,
    pub(crate) admin_job_poll_interval_seconds: u64,
    pub(crate) admin_job_lease_seconds: i64,
    pub(crate) admin_job_max_attempts: i32,
    pub(crate) catalog_publish_job_max_attempts: i32,
}

impl Config {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
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
        let admin_auth_cookie_secure =
            parse_bool_env("ADMIN_AUTH_COOKIE_SECURE", is_production_environment());
        let admin_refresh_token_ttl_hours = parse_i64_env(
            "ADMIN_REFRESH_TOKEN_TTL_HOURS",
            DEFAULT_ADMIN_REFRESH_TOKEN_TTL_HOURS,
        )?;
        if admin_refresh_token_ttl_hours <= 0 {
            anyhow::bail!("ADMIN_REFRESH_TOKEN_TTL_HOURS must be greater than zero");
        }
        let admin_auth_cookie_same_site =
            parse_cookie_same_site_env("ADMIN_AUTH_COOKIE_SAME_SITE", AuthCookieSameSite::Lax)?;
        if matches!(admin_auth_cookie_same_site, AuthCookieSameSite::None)
            && !admin_auth_cookie_secure
        {
            anyhow::bail!(
                "ADMIN_AUTH_COOKIE_SAME_SITE=None requires ADMIN_AUTH_COOKIE_SECURE=true"
            );
        }
        let admin_request_timeout_seconds = parse_f64_env("ADMIN_REQUEST_TIMEOUT_SECONDS", 120.0)?;
        if admin_request_timeout_seconds <= 0.0 {
            anyhow::bail!("ADMIN_REQUEST_TIMEOUT_SECONDS must be greater than zero");
        }
        let admin_upload_request_timeout_seconds =
            parse_f64_env("ADMIN_UPLOAD_REQUEST_TIMEOUT_SECONDS", 3600.0)?;
        if admin_upload_request_timeout_seconds <= 0.0 {
            anyhow::bail!("ADMIN_UPLOAD_REQUEST_TIMEOUT_SECONDS must be greater than zero");
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
        let ffmpeg_max_parallel_renditions = parse_usize_env("FFMPEG_MAX_PARALLEL_RENDITIONS", 1)?;
        if ffmpeg_max_parallel_renditions < 1 {
            anyhow::bail!("FFMPEG_MAX_PARALLEL_RENDITIONS must be at least 1");
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
            antd_internal_token: non_empty_env("ANTD_INTERNAL_TOKEN"),
            antd_payment_mode,
            antd_metadata_payment_mode,
            admin_username,
            admin_password,
            admin_auth_secret,
            admin_auth_ttl_hours,
            admin_auth_cookie_secure,
            catalog_state_path: PathBuf::from(
                env::var("CATALOG_STATE_PATH")
                    .unwrap_or_else(|_| "/tmp/video_catalog/catalog.json".into()),
            ),
            catalog_bootstrap_address: non_empty_env("CATALOG_ADDRESS"),
            cors_allowed_origins: cors_allowed_origins()?,
            bind_addr,
            admin_request_timeout_seconds,
            admin_upload_request_timeout_seconds,
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
            ffmpeg_max_parallel_renditions,
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

impl Config {
    pub(crate) fn admin_refresh_token_ttl_hours(&self) -> i64 {
        parse_i64_env(
            "ADMIN_REFRESH_TOKEN_TTL_HOURS",
            DEFAULT_ADMIN_REFRESH_TOKEN_TTL_HOURS,
        )
        .ok()
        .filter(|ttl_hours| *ttl_hours > 0)
        .unwrap_or(DEFAULT_ADMIN_REFRESH_TOKEN_TTL_HOURS)
    }

    pub(crate) fn admin_auth_cookie_same_site(&self) -> AuthCookieSameSite {
        parse_cookie_same_site_env("ADMIN_AUTH_COOKIE_SAME_SITE", AuthCookieSameSite::Lax)
            .unwrap_or(AuthCookieSameSite::Lax)
    }
}

pub(crate) fn duration_from_secs_f64(seconds: f64) -> StdDuration {
    StdDuration::from_millis((seconds.max(0.001) * 1000.0).ceil() as u64)
}

pub(crate) fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

pub(crate) fn cors_layer(config: &Config) -> anyhow::Result<CorsLayer> {
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
            header::COOKIE,
            header::CONTENT_TYPE,
            header::RANGE,
            HeaderName::from_static("x-request-id"),
            HeaderName::from_static("x-csrf-token"),
        ])
        .allow_credentials(true)
        .expose_headers([HeaderName::from_static("x-request-id")]))
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
            let normalized = value.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                default_value
            } else {
                !matches!(normalized.as_str(), "0" | "false" | "no")
            }
        })
        .unwrap_or(default_value)
}

fn parse_cookie_same_site_env(
    name: &str,
    default_value: AuthCookieSameSite,
) -> anyhow::Result<AuthCookieSameSite> {
    let raw = env::var(name).unwrap_or_else(|_| default_value.as_cookie_value().to_string());
    AuthCookieSameSite::parse(&raw)
        .ok_or_else(|| anyhow::anyhow!("{name} must be one of Strict, Lax, or None"))
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

fn cors_allowed_origins() -> anyhow::Result<Vec<HeaderValue>> {
    let raw = env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost,http://127.0.0.1".into());
    autvid_common::parse_cors_allowed_origins(&raw)
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    #[test]
    fn normalizes_cors_origin_without_paths_or_wildcards() {
        assert_eq!(
            autvid_common::normalize_cors_origin("http://localhost:5173/").unwrap(),
            "http://localhost:5173"
        );
        assert!(autvid_common::normalize_cors_origin("*").is_err());
        assert!(autvid_common::normalize_cors_origin("http://localhost/app").is_err());
    }
}
