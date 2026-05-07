use std::{
    fmt,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use rand::Rng;
use serde::Deserialize;
use tokio::time::sleep;
use tracing::warn;

#[derive(Clone)]
pub(crate) struct AntdRestClient {
    base_url: String,
    client: reqwest::Client,
    internal_token: Option<String>,
    circuit: Arc<CircuitBreaker>,
}

#[derive(Debug)]
struct AntdHttpStatusError {
    method: reqwest::Method,
    path: String,
    status: reqwest::StatusCode,
    body: String,
}

impl fmt::Display for AntdHttpStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} failed: {} {}",
            self.method, self.path, self.status, self.body
        )
    }
}

impl std::error::Error for AntdHttpStatusError {}

#[derive(Debug, Default)]
struct CircuitBreaker {
    consecutive_failures: AtomicUsize,
    opened_until_epoch_ms: AtomicU64,
}

impl CircuitBreaker {
    const FAILURE_THRESHOLD: usize = 5;
    const OPEN_DURATION: Duration = Duration::from_secs(30);

    fn check(&self) -> anyhow::Result<()> {
        let now = epoch_millis();
        let opened_until = self.opened_until_epoch_ms.load(Ordering::Relaxed);
        if opened_until > now {
            anyhow::bail!(
                "Autonomi request circuit is open for {}ms",
                opened_until.saturating_sub(now)
            );
        }
        Ok(())
    }

    fn record_result<T>(&self, result: &anyhow::Result<T>) {
        if result.is_ok() {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            self.opened_until_epoch_ms.store(0, Ordering::Relaxed);
            return;
        }

        let Some(err) = result.as_ref().err() else {
            return;
        };
        if !is_retryable_antd_error(err) {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            return;
        }

        let failures = self
            .consecutive_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if failures >= Self::FAILURE_THRESHOLD {
            let opened_until = epoch_millis()
                .saturating_add(Self::OPEN_DURATION.as_millis().min(u128::from(u64::MAX)) as u64);
            self.opened_until_epoch_ms
                .store(opened_until, Ordering::Relaxed);
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct AntdHealthResponse {
    pub(crate) status: String,
    pub(crate) network: Option<String>,
}

#[derive(Deserialize)]
struct AntdPublicDataResponse {
    data: String,
}

impl AntdRestClient {
    pub(crate) fn new(base_url: &str, internal_token: Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(60))
                .build()?,
            internal_token,
            circuit: Arc::new(CircuitBreaker::default()),
        })
    }

    pub(crate) async fn health(&self) -> anyhow::Result<AntdHealthResponse> {
        self.get_json("/health").await
    }

    pub(crate) async fn data_get_public(&self, address: &str) -> anyhow::Result<Bytes> {
        match self.data_get_public_raw(address).await {
            Ok(bytes) => return Ok(bytes),
            Err(err) if raw_endpoint_unavailable(&err) => {}
            Err(err) => return Err(err),
        }
        self.data_get_public_json(address).await
    }

    async fn data_get_public_raw(&self, address: &str) -> anyhow::Result<Bytes> {
        self.get_bytes(&format!("/v1/data/public/{}/raw", address.trim()))
            .await
    }

    async fn data_get_public_json(&self, address: &str) -> anyhow::Result<Bytes> {
        let payload: AntdPublicDataResponse = self
            .get_json(&format!("/v1/data/public/{}", address.trim()))
            .await?;
        BASE64
            .decode(payload.data)
            .map(Bytes::from)
            .map_err(|err| anyhow::anyhow!("antd returned invalid base64 public data: {err}"))
    }

    async fn get_json<T>(&self, path: &str) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let attempts = 3;
        for attempt in 1..=attempts {
            self.circuit.check()?;
            let result = self.get_json_once(path).await;
            self.circuit.record_result(&result);
            match result {
                Ok(value) => return Ok(value),
                Err(err) if attempt < attempts && is_retryable_antd_error(&err) => {
                    let delay = retry_delay(attempt);
                    warn!(
                        "Autonomi JSON fetch failed on attempt {}/{} for {}: {}; retrying in {}ms",
                        attempt,
                        attempts,
                        path,
                        err,
                        delay.as_millis()
                    );
                    sleep(delay).await;
                }
                Err(err) => return Err(err),
            }
        }
        anyhow::bail!("Autonomi JSON fetch failed for {path}")
    }

    async fn get_json_once<T>(&self, path: &str) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .apply_internal_auth(self.client.get(&url))
            .send()
            .await?;
        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_else(|_| "".to_string());
            return Err(AntdHttpStatusError {
                method: reqwest::Method::GET,
                path: path.to_string(),
                status,
                body,
            }
            .into());
        }

        response.json::<T>().await.map_err(Into::into)
    }

    async fn get_bytes(&self, path: &str) -> anyhow::Result<Bytes> {
        let attempts = 3;
        for attempt in 1..=attempts {
            self.circuit.check()?;
            let result = self.get_bytes_once(path).await;
            self.circuit.record_result(&result);
            match result {
                Ok(value) => return Ok(value),
                Err(err) if attempt < attempts && is_retryable_antd_error(&err) => {
                    let delay = retry_delay(attempt);
                    warn!(
                        "Autonomi byte fetch failed on attempt {}/{} for {}: {}; retrying in {}ms",
                        attempt,
                        attempts,
                        path,
                        err,
                        delay.as_millis()
                    );
                    sleep(delay).await;
                }
                Err(err) => return Err(err),
            }
        }
        anyhow::bail!("Autonomi byte fetch failed for {path}")
    }

    async fn get_bytes_once(&self, path: &str) -> anyhow::Result<Bytes> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .apply_internal_auth(self.client.get(&url))
            .send()
            .await?;
        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_else(|_| "".to_string());
            return Err(AntdHttpStatusError {
                method: reqwest::Method::GET,
                path: path.to_string(),
                status,
                body,
            }
            .into());
        }

        Ok(response.bytes().await?)
    }

    fn apply_internal_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.internal_token.as_deref() {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }
}

fn raw_endpoint_unavailable(err: &anyhow::Error) -> bool {
    if let Some(err) = err.downcast_ref::<AntdHttpStatusError>() {
        return err.path.ends_with("/raw")
            && matches!(
                err.status,
                reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
            );
    }
    false
}

fn is_retryable_antd_error(err: &anyhow::Error) -> bool {
    if let Some(status) = err
        .downcast_ref::<AntdHttpStatusError>()
        .map(|err| err.status)
    {
        return status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
    }
    if let Some(err) = err.downcast_ref::<reqwest::Error>() {
        return err.is_connect() || err.is_timeout() || err.is_body();
    }
    false
}

fn retry_delay(attempt: usize) -> Duration {
    let base = Duration::from_millis(250 * 2_u64.pow((attempt - 1).min(4) as u32));
    jitter_duration(base)
}

fn jitter_duration(base: Duration) -> Duration {
    let factor = rand::thread_rng().gen_range(0.8..=1.2);
    let millis = (base.as_millis() as f64 * factor).round().max(1.0) as u64;
    Duration::from_millis(millis)
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
