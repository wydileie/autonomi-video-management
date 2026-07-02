use std::{sync::Arc, time::Duration};

use autvid_common::antd::{
    raw_endpoint_unavailable, AntdClient, AntdHealthResponse, AntdPublicDataResponse, NoopRecorder,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;

const FETCH_ATTEMPTS: usize = 3;
// Segment reads are latency-sensitive, so start below the admin file-upload delay.
const FETCH_BASE_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(crate) struct AntdRestClient {
    inner: AntdClient,
}

impl AntdRestClient {
    pub(crate) fn new(base_url: &str, internal_token: Option<String>) -> anyhow::Result<Self> {
        Ok(Self {
            inner: AntdClient::new(
                base_url,
                Duration::from_secs(60),
                internal_token,
                Arc::new(NoopRecorder),
            )?,
        })
    }

    pub(crate) async fn health(&self) -> anyhow::Result<AntdHealthResponse> {
        self.inner
            .get_json_retry("/health", FETCH_ATTEMPTS, FETCH_BASE_DELAY)
            .await
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
        self.inner
            .get_bytes_retry(
                &format!("/v1/data/public/{}/raw", address.trim()),
                FETCH_ATTEMPTS,
                FETCH_BASE_DELAY,
            )
            .await
    }

    async fn data_get_public_json(&self, address: &str) -> anyhow::Result<Bytes> {
        let payload: AntdPublicDataResponse = self
            .inner
            .get_json_retry(
                &format!("/v1/data/public/{}", address.trim()),
                FETCH_ATTEMPTS,
                FETCH_BASE_DELAY,
            )
            .await?;
        BASE64
            .decode(payload.data)
            .map(Bytes::from)
            .map_err(|err| anyhow::anyhow!("antd returned invalid base64 public data: {err}"))
    }
}
