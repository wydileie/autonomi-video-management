use std::{path::Path as FsPath, sync::Arc, time::Duration};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{fs as tokio_fs, io::AsyncReadExt, time::sleep};
use tokio_util::io::ReaderStream;

use crate::{
    config::duration_from_secs_f64, metrics::AdminMetrics, MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

#[derive(Clone)]
pub(crate) struct AntdRestClient {
    base_url: String,
    client: reqwest::Client,
    metrics: Arc<AdminMetrics>,
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

#[derive(Deserialize)]
pub(crate) struct AntdDataCostResponse {
    pub(crate) cost: Option<String>,
    pub(crate) chunk_count: Option<i64>,
    pub(crate) estimated_gas_cost_wei: Option<String>,
    pub(crate) payment_mode: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct AntdDataPutResponse {
    pub(crate) address: String,
    pub(crate) cost: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct AntdFilePutResponse {
    pub(crate) address: String,
    pub(crate) byte_size: u64,
    pub(crate) storage_cost_atto: String,
    pub(crate) payment_mode_used: String,
}

impl AntdRestClient {
    pub(crate) fn new(
        base_url: &str,
        timeout_seconds: f64,
        metrics: Arc<AdminMetrics>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(duration_from_secs_f64(timeout_seconds))
                .build()?,
            metrics,
        })
    }

    pub(crate) async fn health(&self) -> anyhow::Result<AntdHealthResponse> {
        self.request_json(reqwest::Method::GET, "/health", Option::<Value>::None)
            .await
    }

    pub(crate) async fn wallet_address(&self) -> anyhow::Result<Value> {
        self.request_json(
            reqwest::Method::GET,
            "/v1/wallet/address",
            Option::<Value>::None,
        )
        .await
    }

    pub(crate) async fn wallet_balance(&self) -> anyhow::Result<Value> {
        self.request_json(
            reqwest::Method::GET,
            "/v1/wallet/balance",
            Option::<Value>::None,
        )
        .await
    }

    pub(crate) async fn wallet_approve(&self) -> anyhow::Result<Value> {
        self.request_json(
            reqwest::Method::POST,
            "/v1/wallet/approve",
            Option::<Value>::None,
        )
        .await
    }

    pub(crate) async fn data_get_public(&self, address: &str) -> anyhow::Result<Vec<u8>> {
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

    pub(crate) async fn data_cost_for_size(
        &self,
        byte_size: usize,
    ) -> anyhow::Result<AntdDataCostResponse> {
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
                        sleep(Duration::from_millis(100 * attempt as u64)).await;
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

    pub(crate) async fn data_put_public(
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

    pub(crate) async fn file_put_public(
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
        let started = std::time::Instant::now();
        let result = async {
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
            serde_json::from_str(&text).map_err(|err| {
                anyhow::anyhow!("POST /v1/file/public returned invalid JSON: {}", err)
            })
        }
        .await;
        self.metrics
            .record_antd_request(started.elapsed(), result.is_ok());
        result
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
        let started = std::time::Instant::now();
        let result = async {
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
            serde_json::from_str(&text).map_err(|err| {
                anyhow::anyhow!("{} {} returned invalid JSON: {}", method, path, err)
            })
        }
        .await;
        self.metrics
            .record_antd_request(started.elapsed(), result.is_ok());
        result
    }

    pub(crate) fn record_upload_retry(&self) {
        self.metrics.record_upload_retry();
    }
}

pub(crate) fn is_missing_file_upload_endpoint(err: &anyhow::Error) -> bool {
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

#[cfg(test)]
mod tests {
    use super::is_missing_file_upload_endpoint;

    #[test]
    fn treats_stream_abort_on_file_upload_route_as_missing_endpoint() {
        let err = anyhow::anyhow!(
            "error sending request for url (http://antd:8082/v1/file/public?payment_mode=auto&verify=true)"
        );
        assert!(is_missing_file_upload_endpoint(&err));

        let err = anyhow::anyhow!("error sending request for url (http://antd:8082/health)");
        assert!(!is_missing_file_upload_endpoint(&err));
    }
}
