use std::env;
use std::time::Duration;

use autvid_common::parse_env;
use axum::http::HeaderValue;

#[derive(Clone)]
pub(crate) struct CacheConfig {
    pub(crate) catalog_ttl: Duration,
    pub(crate) manifest_ttl: Duration,
    pub(crate) segment_ttl: Duration,
    pub(crate) segment_max_bytes: usize,
}

impl CacheConfig {
    pub(crate) fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            catalog_ttl: duration_from_env("STREAM_CATALOG_CACHE_TTL_SECONDS", 10)?,
            manifest_ttl: duration_from_env("STREAM_MANIFEST_CACHE_TTL_SECONDS", 300)?,
            segment_ttl: duration_from_env("STREAM_SEGMENT_CACHE_TTL_SECONDS", 3600)?,
            segment_max_bytes: parse_env("STREAM_SEGMENT_CACHE_MAX_BYTES", 64 * 1024 * 1024)?,
        })
    }

    pub(crate) fn playlist_max_age_seconds(&self) -> u64 {
        self.catalog_ttl.as_secs()
    }

    pub(crate) fn segment_max_age_seconds(&self) -> u64 {
        self.segment_ttl.as_secs()
    }
}

pub(crate) fn cors_allowed_origins() -> anyhow::Result<Vec<HeaderValue>> {
    let raw_origins = env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost,http://127.0.0.1".into());
    autvid_common::parse_cors_allowed_origins(&raw_origins)
}

pub(crate) fn request_timeout_from_env() -> anyhow::Result<Duration> {
    duration_from_env("STREAM_REQUEST_TIMEOUT_SECONDS", 60)
}

fn duration_from_env(name: &str, default_seconds: u64) -> anyhow::Result<Duration> {
    Ok(Duration::from_secs(parse_env(name, default_seconds)?))
}
