use std::{path::Path as FsPath, sync::Arc, time::Duration};

use autvid_common::antd::{AntdClient, AntdMetricsRecorder};
use autvid_common::{is_retryable_antd_error, jitter_duration};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::{json, Value};
use tokio::time::sleep;
use tracing::warn;

use crate::{
    config::duration_from_secs_f64, metrics::AdminMetrics, MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

#[cfg(test)]
pub(crate) use autvid_common::antd::hex_lower;
pub(crate) use autvid_common::antd::{
    is_missing_file_upload_endpoint, AntdDataCostResponse, AntdDataPutResponse,
    AntdFilePutResponse, AntdHealthResponse, AntdPublicDataResponse, AntdWalletAddressResponse,
    AntdWalletApproveResponse, AntdWalletBalanceResponse,
};

const COST_ESTIMATE_ATTEMPTS: usize = 5;

impl AntdMetricsRecorder for AdminMetrics {
    fn record_request(&self, path: &str, latency: Duration, ok: bool) {
        self.record_antd_request(path, latency, ok);
    }

    fn record_upload_retry(&self) {
        AdminMetrics::record_upload_retry(self);
    }
}

#[derive(Clone)]
pub struct AntdRestClient {
    inner: AntdClient,
}

impl AntdRestClient {
    pub fn new(
        base_url: &str,
        timeout_seconds: f64,
        metrics: Arc<AdminMetrics>,
        internal_token: Option<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: AntdClient::new(
                base_url,
                duration_from_secs_f64(timeout_seconds),
                internal_token,
                metrics,
            )?,
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
        for attempt in 1..=COST_ESTIMATE_ATTEMPTS {
            match self.data_cost(&data).await {
                Ok(estimate) => return Ok(estimate),
                Err(err) => {
                    if !is_retryable_antd_error(&err) {
                        return Err(anyhow::anyhow!(
                            "Autonomi cost estimate failed for {quote_size} quote bytes: {err}"
                        ));
                    }
                    if attempt < COST_ESTIMATE_ATTEMPTS {
                        self.record_upload_retry();
                        let delay = jitter_duration(Duration::from_millis(
                            250 * 2_u64.pow((attempt - 1).min(3) as u32),
                        ));
                        warn!(
                            "Autonomi cost estimate failed on attempt {}/{} for {} quote bytes: {}; retrying in {}ms",
                            attempt,
                            COST_ESTIMATE_ATTEMPTS,
                            quote_size,
                            err,
                            delay.as_millis()
                        );
                        sleep(delay).await;
                    }
                    last_error = Some(err);
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
        self.inner
            .file_put_public(path, payment_mode, verify, upload_retries)
            .await
    }

    pub(crate) fn record_upload_retry(&self) {
        self.inner.record_upload_retry();
    }

    async fn request_json<T>(
        &self,
        method: reqwest::Method,
        path: &str,
        json_body: Option<Value>,
    ) -> anyhow::Result<T>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        self.inner.request_json(method, path, json_body).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicU16, AtomicUsize, Ordering},
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

    use autvid_common::AutonomiHttpStatusError;

    use sha2::{Digest, Sha256};

    use super::{hex_lower, is_missing_file_upload_endpoint, AntdRestClient, BASE64};
    use crate::metrics::AdminMetrics;

    #[derive(Clone, Default)]
    struct MockAntdState {
        cost_failures_remaining: Arc<AtomicUsize>,
        cost_failure_status: Arc<AtomicU16>,
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
        let err: anyhow::Error = AutonomiHttpStatusError {
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
    async fn mock_antd_cost_estimate_retries_request_timeout() {
        let mock_state = MockAntdState::default();
        mock_state
            .cost_failures_remaining
            .store(1, Ordering::Relaxed);
        mock_state
            .cost_failure_status
            .store(StatusCode::REQUEST_TIMEOUT.as_u16(), Ordering::Relaxed);
        let base_url = spawn_mock_antd(mock_state.clone()).await;
        let client =
            AntdRestClient::new(&base_url, 5.0, Arc::new(AdminMetrics::default()), None).unwrap();

        let cost = client.data_cost_for_size(3).await.unwrap();

        assert_eq!(cost.cost.as_deref(), Some("321"));
        assert_eq!(mock_state.cost_requests.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn mock_antd_cost_estimate_open_circuit_preserves_last_error() {
        let mock_state = MockAntdState::default();
        mock_state
            .cost_failures_remaining
            .store(10, Ordering::Relaxed);
        let base_url = spawn_mock_antd(mock_state.clone()).await;
        let client =
            AntdRestClient::new(&base_url, 5.0, Arc::new(AdminMetrics::default()), None).unwrap();

        let first_error = match client.data_cost_for_size(3).await {
            Ok(_) => panic!("expected persistent cost estimate failure"),
            Err(err) => err,
        };
        assert!(first_error.to_string().contains("cost unavailable"));

        let second_error = match client.data_cost_for_size(3).await {
            Ok(_) => panic!("expected circuit-open cost estimate failure"),
            Err(err) => err,
        };
        let message = second_error.to_string();
        assert!(message.contains("Autonomi request circuit is open"));
        assert!(message.contains("last retryable error"));
        assert!(message.contains("POST /v1/data/cost failed: 503 Service Unavailable"));
        assert!(message.contains("cost unavailable"));
        assert_eq!(mock_state.cost_requests.load(Ordering::Relaxed), 5);
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
            .route("/v1/data/public/{address}", get(mock_data_get_public))
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
            let status = StatusCode::from_u16(state.cost_failure_status.load(Ordering::Relaxed))
                .unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
            return (status, "cost unavailable").into_response();
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
