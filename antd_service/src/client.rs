use std::sync::Arc;
use std::{env, fs};

use ant_core::data::{
    Client as CoreClient, ClientConfig, CoreNodeConfig, IPDiversityConfig, MultiAddr, NodeMode,
    P2PNode, MAX_WIRE_MESSAGE_SIZE,
};
use tracing::{info, warn};
use zeroize::Zeroize;

use crate::config::non_empty_env;

const DEFAULT_PEERS: &[&str] = &[
    "207.148.94.42:10000",
    "45.77.50.10:10000",
    "66.135.23.83:10000",
    "149.248.9.2:10000",
    "49.12.119.240:10000",
    "5.161.25.133:10000",
    "18.228.202.183:10000",
];

pub(crate) fn init_logging() {
    let level = env::var("ANTD_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    let filter = format!(
        "{level},antd=info,ant_core=info,ant_node=warn,saorsa_core=warn,saorsa_transport=warn"
    );
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}

pub(crate) async fn connect_client() -> anyhow::Result<CoreClient> {
    let peers = bootstrap_peers()?;
    let local_network = is_local_network();
    info!(
        local_network,
        "connecting to Autonomi 2.0 with {} bootstrap peers",
        peers.len(),
    );

    let mut builder = CoreNodeConfig::builder()
        .mode(NodeMode::Client)
        .port(0)
        .ipv6(false)
        .local(local_network)
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
        allow_loopback: local_network,
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
    non_empty_env("AUTONOMI_WALLET_KEY_FILE")
        .and_then(|path| match fs::read_to_string(&path) {
            Ok(value) => Some(value),
            Err(err) => {
                warn!("Could not read AUTONOMI_WALLET_KEY_FILE at {path}: {err}");
                None
            }
        })
        .or_else(|| non_empty_env("AUTONOMI_WALLET_KEY"))
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

fn is_local_network() -> bool {
    env::var("ANTD_NETWORK")
        .ok()
        .as_deref()
        .is_some_and(is_local_network_name)
}

fn is_local_network_name(network: &str) -> bool {
    matches!(
        network.trim().to_ascii_lowercase().as_str(),
        "local" | "devnet" | "development"
    )
}

fn first_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| non_empty_env(name))
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
        .map(|peer| normalize_multiaddr(&peer).parse())
        .collect()
}

pub(crate) fn normalize_multiaddr(peer: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_plain_peers_to_quic_multiaddrs() {
        assert_eq!(
            normalize_multiaddr("207.148.94.42:10000"),
            "/ip4/207.148.94.42/udp/10000/quic"
        );
        assert_eq!(
            normalize_multiaddr("/ip4/127.0.0.1/udp/12000/quic"),
            "/ip4/127.0.0.1/udp/12000/quic"
        );
    }

    #[test]
    fn detects_local_network_modes() {
        assert!(is_local_network_name("local"));
        assert!(is_local_network_name("devnet"));
        assert!(is_local_network_name("development"));
        assert!(!is_local_network_name("arbitrum-one"));
        assert!(!is_local_network_name(""));
    }
}
