use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use bytes::Bytes;
use serde::Deserialize;

#[derive(Clone)]
pub(crate) struct AntdRestClient {
    base_url: String,
    client: reqwest::Client,
    internal_token: Option<String>,
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
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .apply_internal_auth(self.client.get(&url))
            .send()
            .await?;
        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_else(|_| "".to_string());
            anyhow::bail!("GET {path} failed: {status} {body}");
        }

        response.json::<T>().await.map_err(Into::into)
    }

    async fn get_bytes(&self, path: &str) -> anyhow::Result<Bytes> {
        let url = format!("{}{}", self.base_url, path);
        let response = self
            .apply_internal_auth(self.client.get(&url))
            .send()
            .await?;
        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_else(|_| "".to_string());
            anyhow::bail!("GET {path} failed: {status} {body}");
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
    let message = err.to_string();
    message.contains("/raw failed: 404") || message.contains("/raw failed: 405")
}
