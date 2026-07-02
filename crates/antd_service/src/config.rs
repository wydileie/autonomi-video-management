use std::{env, net::SocketAddr, path::PathBuf, time::Duration};

use autvid_common::{cors_allowed_origins_from_env, parse_nonzero_env, secret_env};
use axum::http::HeaderValue;

pub(crate) use autvid_common::non_empty_env;

pub(crate) struct Config {
    pub(crate) bind_addr: SocketAddr,
    pub(crate) network: String,
    pub(crate) internal_token: Option<String>,
    pub(crate) cors_allowed_origins: Vec<HeaderValue>,
    pub(crate) request_timeout: Duration,
    pub(crate) file_upload_request_timeout: Duration,
    pub(crate) json_body_limit_bytes: usize,
    pub(crate) file_upload_max_bytes: u64,
    pub(crate) upload_temp_dir: PathBuf,
    pub(crate) cost_cache_ttl: Duration,
    pub(crate) cost_cache_max_entries: usize,
}

impl Config {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let rest_addr = env::var("ANTD_REST_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".to_string());
        Ok(Self {
            bind_addr: rest_addr.parse()?,
            network: env::var("ANTD_NETWORK").unwrap_or_else(|_| "default".to_string()),
            internal_token: secret_env("ANTD_INTERNAL_TOKEN", "ANTD_INTERNAL_TOKEN_FILE")?,
            cors_allowed_origins: cors_allowed_origins_from_env("ANTD_CORS_ALLOWED_ORIGINS")?,
            request_timeout: duration_from_env("ANTD_REQUEST_TIMEOUT_SECONDS", 150)?,
            file_upload_request_timeout: duration_from_env(
                "ANTD_FILE_UPLOAD_REQUEST_TIMEOUT_SECONDS",
                3600,
            )?,
            json_body_limit_bytes: parse_nonzero_env(
                "ANTD_JSON_BODY_LIMIT_BYTES",
                32 * 1024 * 1024,
            )?,
            file_upload_max_bytes: parse_nonzero_env(
                "ANTD_FILE_UPLOAD_MAX_BYTES",
                20 * 1024 * 1024 * 1024,
            )?,
            upload_temp_dir: PathBuf::from(
                env::var("ANTD_UPLOAD_TEMP_DIR").unwrap_or_else(|_| "/tmp".to_string()),
            ),
            cost_cache_ttl: duration_from_env("ANTD_COST_CACHE_TTL_SECONDS", 60)?,
            cost_cache_max_entries: parse_nonzero_env("ANTD_COST_CACHE_MAX_ENTRIES", 512)?,
        })
    }
}

fn duration_from_env(name: &str, default_seconds: u64) -> anyhow::Result<Duration> {
    Ok(Duration::from_secs(parse_nonzero_env(
        name,
        default_seconds,
    )?))
}
