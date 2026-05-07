use std::{sync::Arc, time::Duration};

use autvid_common::{
    is_retryable_antd_error, jitter_duration, AutonomiHttpStatusError, CircuitBreaker,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
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
            return Err(AutonomiHttpStatusError {
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
            return Err(AutonomiHttpStatusError {
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
    if let Some(err) = err.downcast_ref::<AutonomiHttpStatusError>() {
        return err.path.ends_with("/raw")
            && matches!(
                err.status,
                reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
            );
    }
    false
}

fn retry_delay(attempt: usize) -> Duration {
    // Segment reads are latency-sensitive, so start below the admin file-upload delay.
    let base = Duration::from_millis(250 * 2_u64.pow((attempt - 1).min(4) as u32));
    jitter_duration(base)
}
