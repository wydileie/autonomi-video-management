use std::env;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;

use ant_core::data::{
    Client as CoreClient, ClientConfig, CoreNodeConfig, DataMap, IPDiversityConfig, MultiAddr,
    NodeMode, P2PNode, PaymentMode, MAX_WIRE_MESSAGE_SIZE,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};
use zeroize::Zeroize;

const DEFAULT_PEERS: &[&str] = &[
    "207.148.94.42:10000",
    "45.77.50.10:10000",
    "66.135.23.83:10000",
    "149.248.9.2:10000",
    "49.12.119.240:10000",
    "5.161.25.133:10000",
    "18.228.202.183:10000",
];

#[derive(Clone)]
struct AppState {
    client: Arc<CoreClient>,
    network: String,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    network: String,
    peer_count: usize,
}

#[derive(Serialize)]
struct WalletAddressResponse {
    address: String,
}

#[derive(Serialize)]
struct WalletBalanceResponse {
    balance: String,
    gas_balance: String,
}

#[derive(Serialize)]
struct WalletApproveResponse {
    approved: bool,
}

#[derive(Deserialize)]
struct DataRequest {
    data: String,
    #[serde(default)]
    payment_mode: Option<String>,
}

#[derive(Serialize)]
struct DataCostResponse {
    cost: String,
    file_size: u64,
    chunk_count: usize,
    estimated_gas_cost_wei: String,
    payment_mode: String,
}

#[derive(Serialize)]
struct DataPutResponse {
    address: String,
    chunks_stored: usize,
    payment_mode_used: String,
}

#[derive(Serialize)]
struct DataGetResponse {
    data: String,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
    code: &'static str,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "BAD_REQUEST",
            message: message.into(),
        }
    }

    fn wallet(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "WALLET_ERROR",
            message: message.into(),
        }
    }

    fn from_anyhow(err: anyhow::Error) -> Self {
        Self::from_message(err.to_string())
    }

    fn from_message(message: String) -> Self {
        let code = if is_network_error(&message) {
            "NETWORK_ERROR"
        } else if message.contains("InvalidData") || message.contains("invalid") {
            "BAD_REQUEST"
        } else {
            "AUTONOMI_ERROR"
        };
        let status = match code {
            "NETWORK_ERROR" => StatusCode::BAD_GATEWAY,
            "BAD_REQUEST" => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code,
            message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
                code: self.code,
            }),
        )
            .into_response()
    }
}

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self::from_anyhow(err.into())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    let rest_addr = env::var("ANTD_REST_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".to_string());
    let bind_addr: SocketAddr = rest_addr.parse()?;
    let network = env::var("ANTD_NETWORK").unwrap_or_else(|_| "default".to_string());
    let client = Arc::new(connect_client().await?);

    let state = AppState { client, network };
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/wallet/address", get(wallet_address))
        .route("/v1/wallet/balance", get(wallet_balance))
        .route("/v1/wallet/approve", post(wallet_approve))
        .route("/v1/data/cost", post(data_cost))
        .route("/v1/data/public", post(data_put_public))
        .route("/v1/data/public/{address}", get(data_get_public))
        .layer(CorsLayer::permissive())
        .with_state(state);

    info!("Autonomi 2.0 compatibility gateway listening on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn init_logging() {
    let level = env::var("ANTD_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let filter = format!(
        "{level},antd=info,ant_core=info,ant_node=warn,saorsa_core=warn,saorsa_transport=warn"
    );
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

async fn connect_client() -> anyhow::Result<CoreClient> {
    let peers = bootstrap_peers()?;
    info!(
        "connecting to Autonomi 2.0 with {} bootstrap peers",
        peers.len()
    );

    let mut builder = CoreNodeConfig::builder()
        .mode(NodeMode::Client)
        .port(0)
        .ipv6(false)
        .local(false)
        .max_message_size(MAX_WIRE_MESSAGE_SIZE);

    for peer in peers {
        builder = builder.bootstrap_peer(peer);
    }

    let mut config = builder.build()?;
    config.diversity_config = Some(IPDiversityConfig::permissive());
    let node = Arc::new(P2PNode::new(config).await?);
    start_node_with_warmup(node.clone()).await?;

    let client_config = ClientConfig {
        quote_timeout_secs: env_u64("ANTD_QUOTE_TIMEOUT_SECS", 60),
        store_timeout_secs: env_u64("ANTD_STORE_TIMEOUT_SECS", 120),
        ipv6: false,
        ..ClientConfig::default()
    };

    let client = CoreClient::from_node(node, client_config);
    let evm_network = evm_network();

    let Some(mut private_key) = wallet_key() else {
        warn!("AUTONOMI_WALLET_KEY is not configured; write operations will fail");
        return Ok(client.with_evm_network(evm_network));
    };

    if !private_key.starts_with("0x") {
        private_key = format!("0x{private_key}");
    }

    let wallet = evmlib::wallet::Wallet::new_from_private_key(evm_network, &private_key)?;
    private_key.zeroize();

    Ok(client.with_wallet(wallet))
}

fn wallet_key() -> Option<String> {
    env::var("AUTONOMI_WALLET_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn evm_network() -> evmlib::Network {
    let rpc_url = first_env(&["EVM_RPC_URL", "PROD_EVM_RPC_URL"]);
    let token = first_env(&[
        "EVM_PAYMENT_TOKEN_ADDRESS",
        "PROD_EVM_PAYMENT_TOKEN_ADDRESS",
    ]);
    let vault = first_env(&[
        "EVM_PAYMENT_VAULT_ADDRESS",
        "PROD_EVM_PAYMENT_VAULT_ADDRESS",
    ]);
    if let (Some(rpc_url), Some(token), Some(vault)) = (rpc_url, token, vault) {
        return evmlib::Network::new_custom(&rpc_url, &token, &vault);
    }

    match env::var("EVM_NETWORK")
        .unwrap_or_else(|_| "arbitrum-one".to_string())
        .as_str()
    {
        "arbitrum-sepolia" | "arbitrum-sepolia-test" | "evm-arbitrum-sepolia-test" => {
            evmlib::Network::ArbitrumSepoliaTest
        }
        _ => evmlib::Network::ArbitrumOne,
    }
}

fn first_env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn bootstrap_peers() -> anyhow::Result<Vec<MultiAddr>> {
    let raw = first_env(&["PROD_AUTONOMI_PEERS", "ANTD_PEERS", "ANT_PEERS"]).unwrap_or_default();
    let peers: Vec<String> = if raw.trim().is_empty() {
        DEFAULT_PEERS
            .iter()
            .map(|peer| (*peer).to_string())
            .collect()
    } else {
        raw.split(|c: char| c == ',' || c.is_whitespace())
            .map(str::trim)
            .filter(|peer| !peer.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    };

    peers
        .into_iter()
        .map(|peer| normalize_multiaddr(&peer).parse().map_err(Into::into))
        .collect()
}

fn normalize_multiaddr(peer: &str) -> String {
    if peer.starts_with('/') {
        return peer.to_string();
    }

    match peer.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() => {
            format!("/ip4/{host}/udp/{port}/quic")
        }
        _ => peer.to_string(),
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

async fn start_node_with_warmup(node: Arc<P2PNode>) -> anyhow::Result<()> {
    const START_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);
    const WARMUP_POLL: std::time::Duration = std::time::Duration::from_millis(250);

    let start_task = {
        let node = node.clone();
        tokio::spawn(async move { node.start().await })
    };

    let deadline = tokio::time::Instant::now() + START_DEADLINE;
    loop {
        if !node.connected_peers().await.is_empty() {
            info!("P2P node has at least one peer; DHT bootstrap will continue in the background");
            return Ok(());
        }
        if start_task.is_finished() {
            return Ok(start_task.await??);
        }
        if tokio::time::Instant::now() >= deadline {
            warn!("P2P warmup deadline reached before peers connected");
            return Ok(());
        }
        tokio::time::sleep(WARMUP_POLL).await;
    }
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let peer_count = state.client.network().connected_peers().await.len();
    Json(HealthResponse {
        status: "ok",
        network: state.network,
        peer_count,
    })
}

async fn wallet_address(
    State(state): State<AppState>,
) -> Result<Json<WalletAddressResponse>, ApiError> {
    let wallet = state
        .client
        .wallet()
        .ok_or_else(|| ApiError::wallet("wallet is not configured"))?;
    Ok(Json(WalletAddressResponse {
        address: format!("{:#x}", wallet.address()),
    }))
}

async fn wallet_balance(
    State(state): State<AppState>,
) -> Result<Json<WalletBalanceResponse>, ApiError> {
    let wallet = state
        .client
        .wallet()
        .ok_or_else(|| ApiError::wallet("wallet is not configured"))?;
    let balance = wallet
        .balance_of_tokens()
        .await
        .map_err(|err| ApiError::wallet(err.to_string()))?;
    let gas_balance = wallet
        .balance_of_gas_tokens()
        .await
        .map_err(|err| ApiError::wallet(err.to_string()))?;
    Ok(Json(WalletBalanceResponse {
        balance: balance.to_string(),
        gas_balance: gas_balance.to_string(),
    }))
}

async fn wallet_approve(
    State(state): State<AppState>,
) -> Result<Json<WalletApproveResponse>, ApiError> {
    state
        .client
        .approve_token_spend()
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    Ok(Json(WalletApproveResponse { approved: true }))
}

async fn data_cost(
    State(state): State<AppState>,
    Json(request): Json<DataRequest>,
) -> Result<Json<DataCostResponse>, ApiError> {
    let data = decode_base64(&request.data)?;
    let mode = parse_payment_mode(request.payment_mode.as_deref().unwrap_or("auto"))?;

    let mut file = NamedTempFile::new()?;
    file.write_all(&data)?;

    let estimate = state
        .client
        .estimate_upload_cost(file.path(), mode, None)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;

    Ok(Json(DataCostResponse {
        cost: estimate.storage_cost_atto,
        file_size: estimate.file_size,
        chunk_count: estimate.chunk_count,
        estimated_gas_cost_wei: estimate.estimated_gas_cost_wei,
        payment_mode: format_payment_mode(estimate.payment_mode),
    }))
}

async fn data_put_public(
    State(state): State<AppState>,
    Json(request): Json<DataRequest>,
) -> Result<Json<DataPutResponse>, ApiError> {
    let data = decode_base64(&request.data)?;
    let mode = parse_payment_mode(request.payment_mode.as_deref().unwrap_or("auto"))?;

    let result = state
        .client
        .data_upload_with_mode(Bytes::from(data), mode)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    let address = state
        .client
        .data_map_store(&result.data_map)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;

    Ok(Json(DataPutResponse {
        address: hex::encode(address),
        chunks_stored: result.chunks_stored,
        payment_mode_used: format_payment_mode(result.payment_mode_used),
    }))
}

async fn data_get_public(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> Result<Json<DataGetResponse>, ApiError> {
    let address = hex_to_address(&address)?;
    let data_map = state
        .client
        .data_map_fetch(&address)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    let root_map = resolve_data_map(&state.client, data_map)?;
    let content = state
        .client
        .data_download(&root_map)
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    Ok(Json(DataGetResponse {
        data: BASE64.encode(content),
    }))
}

fn decode_base64(value: &str) -> Result<Vec<u8>, ApiError> {
    BASE64
        .decode(value)
        .map_err(|err| ApiError::bad_request(format!("invalid base64 data: {err}")))
}

fn parse_payment_mode(mode: &str) -> Result<PaymentMode, ApiError> {
    match mode.trim().to_lowercase().as_str() {
        "auto" => Ok(PaymentMode::Auto),
        "merkle" => Ok(PaymentMode::Merkle),
        "single" => Ok(PaymentMode::Single),
        other => Err(ApiError::bad_request(format!(
            "invalid payment_mode {other:?}; use auto, merkle, or single"
        ))),
    }
}

fn format_payment_mode(mode: PaymentMode) -> String {
    match mode {
        PaymentMode::Auto => "auto".to_string(),
        PaymentMode::Merkle => "merkle".to_string(),
        PaymentMode::Single => "single".to_string(),
    }
}

fn hex_to_address(value: &str) -> Result<[u8; 32], ApiError> {
    let bytes = hex::decode(value.trim())
        .map_err(|err| ApiError::bad_request(format!("invalid hex address: {err}")))?;
    bytes
        .try_into()
        .map_err(|_| ApiError::bad_request("address must be 32 bytes"))
}

fn resolve_data_map(inner: &CoreClient, data_map: DataMap) -> Result<DataMap, ApiError> {
    if !data_map.is_child() {
        return Ok(data_map);
    }

    let handle = tokio::runtime::Handle::current();
    tokio::task::block_in_place(|| {
        let fetch = |batch: &[(usize, xor_name::XorName)]| -> Result<
            Vec<(usize, bytes::Bytes)>,
            self_encryption::Error,
        > {
            let batch_owned: Vec<(usize, xor_name::XorName)> = batch.to_vec();
            handle.block_on(async {
                let mut results = Vec::with_capacity(batch_owned.len());
                for (idx, hash) in batch_owned {
                    let chunk = inner
                        .chunk_get(&hash.0)
                        .await
                        .map_err(|err| {
                            self_encryption::Error::Generic(format!(
                                "DataMap chunk_get failed: {err}"
                            ))
                        })?
                        .ok_or_else(|| {
                            self_encryption::Error::Generic(format!(
                                "DataMap chunk not found: {}",
                                hex::encode(hash.0)
                            ))
                        })?;
                    results.push((idx, chunk.content));
                }
                Ok(results)
            })
        };
        self_encryption::get_root_data_map_parallel(data_map, &fetch)
    })
    .map_err(|err| ApiError::from_message(format!("DataMap resolution failed: {err}")))
}

fn is_network_error(message: &str) -> bool {
    [
        "InsufficientPeers",
        "Found 0 peers",
        "need 7",
        "DHT returned no peers",
        "Failed to connect",
        "bootstrap",
        "Timeout",
        "timeout",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}
