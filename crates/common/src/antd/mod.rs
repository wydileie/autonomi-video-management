//! Shared HTTP client core for the antd (Autonomi gateway) REST API.
//!
//! `AntdClient` owns the request plumbing every service needs: bearer auth,
//! circuit breaking, retry loops with jittered backoff, and metrics hooks via
//! [`AntdMetricsRecorder`]. Services wrap it with their own endpoint methods
//! and retry policies, so their observable behavior is chosen at the call
//! site rather than baked in here.

mod recorder;
mod types;

use std::{path::Path as FsPath, sync::Arc, time::Duration};

use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{fs as tokio_fs, io::AsyncReadExt, time::sleep};
use tokio_util::io::ReaderStream;
use tracing::warn;

use crate::resilience::{
    is_retryable_antd_error, jitter_duration, AutonomiHttpStatusError, CircuitBreaker,
};

pub use recorder::{AntdMetricsRecorder, NoopRecorder};
pub use types::{
    AntdDataCostResponse, AntdDataPutResponse, AntdFilePutResponse, AntdHealthResponse,
    AntdPublicDataResponse, AntdWalletAddressResponse, AntdWalletApproveResponse,
    AntdWalletBalanceResponse,
};

#[derive(Clone)]
pub struct AntdClient {
    base_url: String,
    client: reqwest::Client,
    internal_token: Option<String>,
    payment_approval: Option<String>,
    payment_lease: Option<String>,
    circuit: Arc<CircuitBreaker>,
    recorder: Arc<dyn AntdMetricsRecorder>,
}

impl AntdClient {
    pub fn new(
        base_url: &str,
        timeout: Duration,
        internal_token: Option<String>,
        recorder: Arc<dyn AntdMetricsRecorder>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(timeout)
                .build()?,
            internal_token,
            payment_approval: None,
            payment_lease: None,
            circuit: Arc::new(CircuitBreaker::default()),
            recorder,
        })
    }

    pub fn with_lease(&self, key: &str) -> Self {
        let mut client = self.clone();
        client.payment_lease = Some(key.into());
        client
    }

    pub fn with_approval(&self, quote_id: &str) -> Self {
        let mut client = self.clone();
        client.payment_approval = Some(quote_id.to_string());
        client
    }

    pub async fn file_cost(
        &self,
        path: &FsPath,
        payment_mode: &str,
    ) -> anyhow::Result<crate::payments::ContentQuote> {
        let (_, sha256) = sha256_file_async(path).await?;
        self.send_file(path, payment_mode, false, &sha256, "/v1/file/cost")
            .await
    }

    pub fn record_upload_retry(&self) {
        self.recorder.record_upload_retry();
    }

    /// Single-shot instrumented JSON request: circuit check, bearer auth,
    /// metrics recording. Callers add retry loops where appropriate.
    pub async fn request_json<T>(
        &self,
        method: reqwest::Method,
        path: &str,
        json_body: Option<Value>,
    ) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.circuit.check()?;
        let metric_path = metrics_path(path);
        let started = std::time::Instant::now();
        let result = async {
            let url = format!("{}{}", self.base_url, path);
            let mut request = self.apply_internal_auth(self.client.request(method.clone(), url));
            if let Some(body) = json_body {
                request = request.json(&body);
            }
            let response = request.send().await?;
            let status = response.status();
            let text = String::from_utf8(
                read_response_limited(response, 32 * 1024 * 1024)
                    .await?
                    .to_vec(),
            )?;
            if !status.is_success() {
                return Err(AutonomiHttpStatusError {
                    method: method.clone(),
                    path: path.to_string(),
                    status,
                    body: text,
                }
                .into());
            }
            serde_json::from_str(&text).map_err(|err| {
                anyhow::anyhow!("{} {} returned invalid JSON: {}", method, path, err)
            })
        }
        .await;
        self.circuit.record_result(&result);
        self.recorder
            .record_request(&metric_path, started.elapsed(), result.is_ok());
        result
    }

    /// GET a JSON payload with retries on retryable errors; delay for attempt
    /// `n` is `jitter(base_delay * 2^(n-1))` capped at 16x the base.
    pub async fn get_json_retry<T>(
        &self,
        path: &str,
        attempts: usize,
        base_delay: Duration,
    ) -> anyhow::Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        for attempt in 1..=attempts {
            let result = self
                .request_json::<T>(reqwest::Method::GET, path, None)
                .await;
            match result {
                Ok(value) => return Ok(value),
                Err(err) if attempt < attempts && is_retryable_antd_error(&err) => {
                    let delay = retry_delay(base_delay, attempt);
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

    /// GET raw bytes with the same retry semantics as [`Self::get_json_retry`].
    pub async fn get_bytes_retry(
        &self,
        path: &str,
        attempts: usize,
        base_delay: Duration,
    ) -> anyhow::Result<Bytes> {
        for attempt in 1..=attempts {
            let result = self.get_bytes_once(path).await;
            match result {
                Ok(value) => return Ok(value),
                Err(err) if attempt < attempts && is_retryable_antd_error(&err) => {
                    let delay = retry_delay(base_delay, attempt);
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
        self.circuit.check()?;
        let metric_path = metrics_path(path);
        let started = std::time::Instant::now();
        let result = async {
            let url = format!("{}{}", self.base_url, path);
            let response = self
                .apply_internal_auth(self.client.get(&url))
                .send()
                .await?;
            let status = response.status();
            if !status.is_success() {
                let body =
                    String::from_utf8_lossy(&read_response_limited(response, 64 * 1024).await?)
                        .into_owned();
                return Err(AutonomiHttpStatusError {
                    method: reqwest::Method::GET,
                    path: path.to_string(),
                    status,
                    body,
                }
                .into());
            }
            read_response_limited(response, 32 * 1024 * 1024).await
        }
        .await;
        self.circuit.record_result(&result);
        self.recorder
            .record_request(&metric_path, started.elapsed(), result.is_ok());
        result
    }

    /// Upload a file with streaming, SHA-256 integrity header, and a slow
    /// backoff retry loop (full-file resends are expensive).
    pub async fn file_put_public(
        &self,
        path: &FsPath,
        payment_mode: &str,
        verify: bool,
        upload_retries: usize,
    ) -> anyhow::Result<AntdFilePutResponse> {
        let (_, sha256) = sha256_file_async(path).await?;
        let attempts = upload_retries.max(1);
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self
                .send_file(path, payment_mode, verify, &sha256, "/v1/file/public")
                .await
            {
                Ok(result) => return Ok(result),
                Err(err) if attempt < attempts && is_retryable_antd_error(&err) => {
                    // Full file uploads back off more slowly to avoid re-sending large streams.
                    let delay = jitter_duration(Duration::from_secs(
                        2_u64.pow((attempt - 1).min(4) as u32),
                    ));
                    warn!(
                        "Autonomi file upload failed on attempt {}/{} for {}: {}; retrying in {}ms",
                        attempt,
                        attempts,
                        path.display(),
                        err,
                        delay.as_millis()
                    );
                    self.recorder.record_upload_retry();
                    last_error = Some(err);
                    sleep(delay).await;
                }
                Err(err) => return Err(err),
            }
        }

        Err(last_error
            .map(|err| {
                anyhow::anyhow!("Autonomi file upload failed after {attempts} attempt(s): {err}")
            })
            .unwrap_or_else(|| {
                anyhow::anyhow!("Autonomi file upload failed after {attempts} attempt(s)")
            }))
    }

    async fn send_file<T: for<'de> Deserialize<'de>>(
        &self,
        path: &FsPath,
        payment_mode: &str,
        verify: bool,
        sha256: &str,
        endpoint: &str,
    ) -> anyhow::Result<T> {
        self.circuit.check()?;
        let file = tokio_fs::File::open(path).await?;
        let stream = ReaderStream::new(file);
        let url = format!(
            "{}{endpoint}?payment_mode={payment_mode}&verify={}",
            self.base_url, verify
        );
        let started = std::time::Instant::now();
        let result = async {
            let request = self
                .apply_internal_auth(self.client.post(url))
                .header("content-type", "application/octet-stream")
                .header("x-content-sha256", sha256)
                .body(reqwest::Body::wrap_stream(stream));
            let response = request.send().await?;
            let status = response.status();
            let text = String::from_utf8(
                read_response_limited(response, 32 * 1024 * 1024)
                    .await?
                    .to_vec(),
            )?;
            if !status.is_success() {
                return Err(AutonomiHttpStatusError {
                    method: reqwest::Method::POST,
                    path: endpoint.to_string(),
                    status,
                    body: text,
                }
                .into());
            }
            serde_json::from_str(&text)
                .map_err(|err| anyhow::anyhow!("POST {endpoint} returned invalid JSON: {}", err))
        }
        .await;
        self.circuit.record_result(&result);
        self.recorder
            .record_request(endpoint, started.elapsed(), result.is_ok());
        result
    }

    fn apply_internal_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let request = match &self.payment_lease {
            Some(key) => request.header("x-payment-lease", key),
            None => request,
        };
        let request = match &self.payment_approval {
            Some(id) => request.header("x-payment-approval", id),
            None => request,
        };
        match self.internal_token.as_deref() {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }
}

fn retry_delay(base_delay: Duration, attempt: usize) -> Duration {
    let multiplier = 2_u64.pow((attempt - 1).min(4) as u32);
    jitter_duration(base_delay.saturating_mul(multiplier as u32))
}

fn metrics_path(path: &str) -> String {
    if path.starts_with("/v1/data/public/") {
        "/v1/data/public/:address".to_string()
    } else {
        path.to_string()
    }
}

/// True when a `/raw` byte endpoint is not implemented by the antd server,
/// signalling the caller to fall back to the base64 JSON endpoint.
pub fn raw_endpoint_unavailable(err: &anyhow::Error) -> bool {
    if let Some(err) = err.downcast_ref::<AutonomiHttpStatusError>() {
        return err.path.ends_with("/raw")
            && matches!(
                err.status,
                reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::METHOD_NOT_ALLOWED
            );
    }
    false
}

/// True when the antd server predates the streaming file upload endpoint.
pub fn is_missing_file_upload_endpoint(err: &anyhow::Error) -> bool {
    if let Some(status) = err
        .downcast_ref::<AutonomiHttpStatusError>()
        .map(|err| err.status)
    {
        return matches!(
            status,
            reqwest::StatusCode::NOT_FOUND
                | reqwest::StatusCode::METHOD_NOT_ALLOWED
                | reqwest::StatusCode::NOT_IMPLEMENTED
        );
    }

    let message = err.to_string();
    message.contains("/v1/file/public")
        && (message.contains(" 404 ") || message.contains(" 405 ") || message.contains(" 501 "))
}

pub async fn sha256_file_async(path: &FsPath) -> anyhow::Result<(u64, String)> {
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

pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Checks advertised length and every received chunk, including chunked responses.
pub async fn read_response_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> anyhow::Result<Bytes> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        anyhow::bail!("gateway response exceeds {limit} byte limit");
    }
    let mut bytes = bytes::BytesMut::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            anyhow::bail!("gateway response exceeds {limit} byte limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes.freeze())
}

#[cfg(test)]
mod response_limit_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    async fn response(raw: &'static [u8]) -> reqwest::Response {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await;
            stream.write_all(raw).await.expect("response");
        });
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client")
            .get(format!("http://{address}"))
            .send()
            .await
            .expect("request")
    }
    #[tokio::test]
    async fn rejects_advertised_and_chunked_overruns() {
        let advertised =
            response(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000000\r\nConnection: close\r\n\r\n")
                .await;
        assert!(read_response_limited(advertised, 4).await.is_err());
        let chunked=response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n").await;
        assert!(read_response_limited(chunked, 4).await.is_err());
        let exact =
            response(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd")
                .await;
        assert_eq!(
            &read_response_limited(exact, 4).await.expect("exact limit")[..],
            b"abcd"
        );
    }
}
