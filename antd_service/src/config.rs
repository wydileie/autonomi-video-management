use std::{env, net::SocketAddr, time::Duration};

pub(crate) struct Config {
    pub(crate) bind_addr: SocketAddr,
    pub(crate) network: String,
    pub(crate) internal_token: Option<String>,
    pub(crate) request_timeout: Duration,
    pub(crate) file_upload_request_timeout: Duration,
    pub(crate) json_body_limit_bytes: usize,
    pub(crate) cost_cache_ttl: Duration,
    pub(crate) cost_cache_max_entries: usize,
}

impl Config {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let rest_addr = env::var("ANTD_REST_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".to_string());
        Ok(Self {
            bind_addr: rest_addr.parse()?,
            network: env::var("ANTD_NETWORK").unwrap_or_else(|_| "default".to_string()),
            internal_token: env::var("ANTD_INTERNAL_TOKEN")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            request_timeout: duration_from_env("ANTD_REQUEST_TIMEOUT_SECONDS", 60)?,
            file_upload_request_timeout: duration_from_env(
                "ANTD_FILE_UPLOAD_REQUEST_TIMEOUT_SECONDS",
                3600,
            )?,
            json_body_limit_bytes: usize_from_env("ANTD_JSON_BODY_LIMIT_BYTES", 32 * 1024 * 1024)?,
            cost_cache_ttl: duration_from_env("ANTD_COST_CACHE_TTL_SECONDS", 60)?,
            cost_cache_max_entries: usize_from_env("ANTD_COST_CACHE_MAX_ENTRIES", 512)?,
        })
    }
}

fn usize_from_env(name: &str, default_value: usize) -> anyhow::Result<usize> {
    let value = env::var(name)
        .unwrap_or_else(|_| default_value.to_string())
        .parse::<usize>()
        .map_err(|err| anyhow::anyhow!("{name} must be an integer: {err}"))?;
    if value == 0 {
        anyhow::bail!("{name} must be greater than zero");
    }
    Ok(value)
}

fn duration_from_env(name: &str, default_seconds: u64) -> anyhow::Result<Duration> {
    let seconds = env::var(name)
        .unwrap_or_else(|_| default_seconds.to_string())
        .parse::<u64>()
        .map_err(|err| anyhow::anyhow!("{name} must be an integer number of seconds: {err}"))?;
    if seconds == 0 {
        anyhow::bail!("{name} must be greater than zero");
    }
    Ok(Duration::from_secs(seconds))
}
