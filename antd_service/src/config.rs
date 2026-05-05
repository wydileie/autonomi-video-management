use std::{env, net::SocketAddr, time::Duration};

pub(crate) struct Config {
    pub(crate) bind_addr: SocketAddr,
    pub(crate) network: String,
    pub(crate) request_timeout: Duration,
    pub(crate) file_upload_request_timeout: Duration,
}

impl Config {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        let rest_addr = env::var("ANTD_REST_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".to_string());
        Ok(Self {
            bind_addr: rest_addr.parse()?,
            network: env::var("ANTD_NETWORK").unwrap_or_else(|_| "default".to_string()),
            request_timeout: duration_from_env("ANTD_REQUEST_TIMEOUT_SECONDS", 60)?,
            file_upload_request_timeout: duration_from_env(
                "ANTD_FILE_UPLOAD_REQUEST_TIMEOUT_SECONDS",
                3600,
            )?,
        })
    }
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
