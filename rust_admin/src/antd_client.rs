use std::{
    fmt,
    path::Path as FsPath,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rand::Rng;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{fs as tokio_fs, io::AsyncReadExt, time::sleep};
use tokio_util::io::ReaderStream;
use tracing::warn;

use crate::{
    config::duration_from_secs_f64, metrics::AdminMetrics, MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

#[derive(Clone)]
pub(crate) struct AntdRestClient {
    base_url: String,
    client: reqwest::Client,
    metrics: Arc<AdminMetrics>,
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

#[derive(Debug, Deserialize)]
pub(crate) struct AntdWalletAddressResponse {
    pub(crate) address: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AntdWalletBalanceResponse {
    pub(crate) balance: String,
    pub(crate) gas_balance: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AntdWalletApproveResponse {
    pub(crate) approved: bool,
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
        internal_token: Option<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(duration_from_secs_f64(timeout_seconds))
                .build()?,
            metrics,
            internal_token,
            circuit: Arc::new(CircuitBreaker::default()),
        })
    }

    pub(crate) async fn health(&self) -> anyhow::Result<AntdHealthResponse> {
        self.request_json(reqwest::Method::GET, "/health", Option::<Value>::None)
            .await
    }

    pub(crate) async fn wallet_address(&self) -> anyhow::Result<AntdWalletAddressResponse> {
        self.request_json(
            reqwest::Method::GET,
            "/v1/wallet/address",
            Option::<Value>::None,
        )
        .await
    }

    pub(crate) async fn wallet_balance(&self) -> anyhow::Result<AntdWalletBalanceResponse> {
        self.request_json(
            reqwest::Method::GET,
            "/v1/wallet/balance",
            Option::<Value>::None,
        )
        .await
    }

    pub(crate) async fn wallet_approve(&self) -> anyhow::Result<AntdWalletApproveResponse> {
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
        let data = vec![0_u8; quote_size];
        let mut last_error = None;
        for attempt in 1..=3 {
            match self.data_cost(&data).await {
                Ok(estimate) => return Ok(estimate),
                Err(err) => {
                    if !is_retryable_antd_error(&err) {
                        return Err(anyhow::anyhow!(
                            "Autonomi cost estimate failed for {quote_size} quote bytes: {err}"
                        ));
                    }
                    last_error = Some(err);
                    if attempt < 3 {
                        self.record_upload_retry();
                        sleep(jitter_duration(Duration::from_millis(100 * attempt as u64))).await;
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
        upload_retries: usize,
    ) -> anyhow::Result<AntdFilePutResponse> {
        let (_, sha256) = sha256_file_async(path).await?;
        let attempts = upload_retries.max(1);
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self
                .file_put_public_once(path, payment_mode, verify, &sha256)
                .await
            {
                Ok(result) => return Ok(result),
                Err(err) if attempt < attempts && is_retryable_antd_error(&err) => {
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
                    self.record_upload_retry();
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

    async fn file_put_public_once(
        &self,
        path: &FsPath,
        payment_mode: &str,
        verify: bool,
        sha256: &str,
    ) -> anyhow::Result<AntdFilePutResponse> {
        self.circuit.check()?;
        let file = tokio_fs::File::open(path).await?;
        let stream = ReaderStream::new(file);
        let url = format!(
            "{}/v1/file/public?payment_mode={payment_mode}&verify={}",
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
            let text = response.text().await?;
            if !status.is_success() {
                return Err(AntdHttpStatusError {
                    method: reqwest::Method::POST,
                    path: "/v1/file/public".to_string(),
                    status,
                    body: text,
                }
                .into());
            }
            serde_json::from_str(&text).map_err(|err| {
                anyhow::anyhow!("POST /v1/file/public returned invalid JSON: {}", err)
            })
        }
        .await;
        self.circuit.record_result(&result);
        self.metrics
            .record_antd_request("/v1/file/public", started.elapsed(), result.is_ok());
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
            let text = response.text().await?;
            if !status.is_success() {
                return Err(AntdHttpStatusError {
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
        self.metrics
            .record_antd_request(&metric_path, started.elapsed(), result.is_ok());
        result
    }

    pub(crate) fn record_upload_retry(&self) {
        self.metrics.record_upload_retry();
    }

    fn apply_internal_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.internal_token.as_deref() {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }
}

pub(crate) fn jitter_duration(base: Duration) -> Duration {
    if base.is_zero() {
        return base;
    }
    let factor = rand::thread_rng().gen_range(0.8..=1.2);
    let millis = (base.as_millis() as f64 * factor).round().max(1.0) as u64;
    Duration::from_millis(millis)
}

fn metrics_path(path: &str) -> String {
    if path.starts_with("/v1/data/public/") {
        "/v1/data/public/:address".to_string()
    } else {
        path.to_string()
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

pub(crate) fn is_missing_file_upload_endpoint(err: &anyhow::Error) -> bool {
    if let Some(status) = err
        .downcast_ref::<AntdHttpStatusError>()
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

pub(crate) fn is_retryable_antd_error(err: &anyhow::Error) -> bool {
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
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use axum::{
        body::{to_bytes, Body},
        extract::{Path, Query, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use base64::Engine;
    use tokio::fs as tokio_fs;
    use tokio::sync::Mutex;
    use uuid::Uuid;

    use super::{
        hex_lower, is_missing_file_upload_endpoint, AntdHttpStatusError, AntdRestClient, Digest,
        Sha256, BASE64,
    };
    use crate::metrics::AdminMetrics;

    #[derive(Clone, Default)]
    struct MockAntdState {
        cost_failures_remaining: Arc<AtomicUsize>,
        cost_payload_hashes: Arc<Mutex<Vec<String>>>,
        cost_requests: Arc<AtomicUsize>,
        last_file_upload: Arc<Mutex<Option<FileUploadObservation>>>,
    }

    #[derive(Clone, Debug)]
    struct FileUploadObservation {
        body: Vec<u8>,
        content_type: Option<String>,
        payment_mode: Option<String>,
        verify: Option<String>,
        sha256: Option<String>,
    }

    #[test]
    fn treats_only_missing_file_upload_statuses_as_missing_endpoint() {
        let err: anyhow::Error = AntdHttpStatusError {
            method: reqwest::Method::POST,
            path: "/v1/file/public".to_string(),
            status: reqwest::StatusCode::NOT_FOUND,
            body: "missing".to_string(),
        }
        .into();
        assert!(is_missing_file_upload_endpoint(&err));

        let err = anyhow::anyhow!(
            "error sending request for url (http://antd:8082/v1/file/public?payment_mode=auto&verify=true)"
        );
        assert!(!is_missing_file_upload_endpoint(&err));
    }

    #[tokio::test]
    async fn mock_antd_client_exercises_json_wallet_data_and_file_routes() {
        let mock_state = MockAntdState::default();
        let base_url = spawn_mock_antd(mock_state.clone()).await;
        let metrics = Arc::new(AdminMetrics::default());
        let client = AntdRestClient::new(&base_url, 5.0, metrics.clone(), None).unwrap();

        let health = client.health().await.unwrap();
        assert_eq!(health.status, "ok");
        assert_eq!(health.network.as_deref(), Some("mocknet"));

        let wallet = client.wallet_address().await.unwrap();
        assert_eq!(wallet.address, "0xabc123");
        let balance = client.wallet_balance().await.unwrap();
        assert_eq!(balance.balance, "1000");
        assert_eq!(balance.gas_balance, "2000");
        assert!(client.wallet_approve().await.unwrap().approved);

        let cost = client.data_cost_for_size(1).await.unwrap();
        assert_eq!(cost.cost.as_deref(), Some("321"));
        assert_eq!(cost.chunk_count, Some(1));
        assert_eq!(cost.estimated_gas_cost_wei.as_deref(), Some("654"));
        assert_eq!(cost.payment_mode.as_deref(), Some("auto"));

        let put = client.data_put_public(b"manifest", "merkle").await.unwrap();
        assert_eq!(put.address, "data-address");
        assert_eq!(put.cost.as_deref(), Some("111"));

        let fetched = client.data_get_public("segment-address").await.unwrap();
        assert_eq!(fetched, b"payload:segment-address");

        let source_path =
            std::env::temp_dir().join(format!("autvid_antd_client_file_{}.bin", Uuid::new_v4()));
        tokio_fs::write(&source_path, b"file upload bytes")
            .await
            .unwrap();
        let file = client
            .file_put_public(&source_path, "auto", true, 1)
            .await
            .unwrap();
        let _ = tokio_fs::remove_file(&source_path).await;
        assert_eq!(file.address, "file-address");
        assert_eq!(file.byte_size, 17);
        assert_eq!(file.storage_cost_atto, "222");
        assert_eq!(file.payment_mode_used, "auto");

        let upload = mock_state.last_file_upload.lock().await.clone().unwrap();
        assert_eq!(upload.body, b"file upload bytes");
        assert_eq!(
            upload.content_type.as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(upload.payment_mode.as_deref(), Some("auto"));
        assert_eq!(upload.verify.as_deref(), Some("true"));
        let mut hasher = Sha256::new();
        hasher.update(b"file upload bytes");
        let expected_sha = hex_lower(&hasher.finalize());
        assert_eq!(upload.sha256.as_deref(), Some(expected_sha.as_str()));

        let rendered = metrics.render_prometheus();
        assert!(rendered.contains("autvid_admin_antd_requests_total{service=\"rust_admin\"} 8"));
        assert!(
            rendered.contains("autvid_admin_antd_request_errors_total{service=\"rust_admin\"} 0")
        );
    }

    #[tokio::test]
    async fn mock_antd_cost_estimate_retries_transient_failures() {
        let mock_state = MockAntdState::default();
        mock_state
            .cost_failures_remaining
            .store(2, Ordering::Relaxed);
        let base_url = spawn_mock_antd(mock_state.clone()).await;
        let client =
            AntdRestClient::new(&base_url, 5.0, Arc::new(AdminMetrics::default()), None).unwrap();

        let cost = client.data_cost_for_size(3).await.unwrap();

        assert_eq!(cost.cost.as_deref(), Some("321"));
        assert_eq!(mock_state.cost_requests.load(Ordering::Relaxed), 3);
        let hashes = mock_state.cost_payload_hashes.lock().await.clone();
        assert_eq!(hashes.len(), 3);
        assert!(hashes.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[tokio::test]
    async fn mock_antd_client_rejects_invalid_base64_downloads() {
        let base_url = spawn_mock_antd(MockAntdState::default()).await;
        let client =
            AntdRestClient::new(&base_url, 5.0, Arc::new(AdminMetrics::default()), None).unwrap();

        let err = client.data_get_public("bad-base64").await.unwrap_err();

        assert!(err.to_string().contains("invalid base64 public data"));
    }

    async fn spawn_mock_antd(state: MockAntdState) -> String {
        let app = Router::new()
            .route("/health", get(mock_health))
            .route("/v1/wallet/address", get(mock_wallet_address))
            .route("/v1/wallet/balance", get(mock_wallet_balance))
            .route("/v1/wallet/approve", post(mock_wallet_approve))
            .route("/v1/data/cost", post(mock_data_cost))
            .route("/v1/data/public", post(mock_data_put_public))
            .route("/v1/data/public/:address", get(mock_data_get_public))
            .route("/v1/file/public", post(mock_file_put_public))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    async fn mock_health() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "status": "ok",
            "network": "mocknet",
        }))
    }

    async fn mock_wallet_address() -> Json<serde_json::Value> {
        Json(serde_json::json!({ "address": "0xabc123" }))
    }

    async fn mock_wallet_balance() -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "balance": "1000",
            "gas_balance": "2000",
        }))
    }

    async fn mock_wallet_approve() -> Json<serde_json::Value> {
        Json(serde_json::json!({ "approved": true }))
    }

    async fn mock_data_cost(
        State(state): State<MockAntdState>,
        Json(body): Json<serde_json::Value>,
    ) -> axum::response::Response {
        state.cost_requests.fetch_add(1, Ordering::Relaxed);
        let data = body
            .get("data")
            .and_then(serde_json::Value::as_str)
            .unwrap();
        let decoded = BASE64.decode(data).unwrap();
        assert!(decoded.len() >= 3);
        let mut hasher = Sha256::new();
        hasher.update(&decoded);
        state
            .cost_payload_hashes
            .lock()
            .await
            .push(hex_lower(&hasher.finalize()));
        if state
            .cost_failures_remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return (StatusCode::SERVICE_UNAVAILABLE, "cost unavailable").into_response();
        }

        Json(serde_json::json!({
            "cost": "321",
            "chunk_count": 1,
            "estimated_gas_cost_wei": "654",
            "payment_mode": "auto",
        }))
        .into_response()
    }

    async fn mock_data_put_public(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
        assert_eq!(
            body.get("payment_mode").and_then(serde_json::Value::as_str),
            Some("merkle")
        );
        assert_eq!(
            BASE64
                .decode(
                    body.get("data")
                        .and_then(serde_json::Value::as_str)
                        .unwrap()
                )
                .unwrap(),
            b"manifest"
        );
        Json(serde_json::json!({
            "address": "data-address",
            "cost": "111",
        }))
    }

    async fn mock_data_get_public(Path(address): Path<String>) -> Json<serde_json::Value> {
        if address == "bad-base64" {
            return Json(serde_json::json!({ "data": "%%%" }));
        }

        Json(serde_json::json!({
            "data": BASE64.encode(format!("payload:{address}")),
        }))
    }

    async fn mock_file_put_public(
        State(state): State<MockAntdState>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
        body: Body,
    ) -> Json<serde_json::Value> {
        let body = to_bytes(body, usize::MAX).await.unwrap().to_vec();
        let mut hasher = Sha256::new();
        hasher.update(&body);
        let expected_sha = hex_lower(&hasher.finalize());
        assert_eq!(
            headers
                .get("x-content-sha256")
                .and_then(|value| value.to_str().ok()),
            Some(expected_sha.as_str())
        );
        *state.last_file_upload.lock().await = Some(FileUploadObservation {
            body: body.clone(),
            content_type: headers
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            payment_mode: query.get("payment_mode").cloned(),
            verify: query.get("verify").cloned(),
            sha256: headers
                .get("x-content-sha256")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
        });

        Json(serde_json::json!({
            "address": "file-address",
            "byte_size": body.len(),
            "chunks_stored": 2,
            "total_chunks": 2,
            "chunks_failed": 0,
            "storage_cost_atto": "222",
            "estimated_gas_cost_wei": "333",
            "payment_mode_used": "auto",
            "verified": true,
        }))
    }
}
