use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

use tower_http::cors::CorsLayer;
use tracing::info;

use crate::client::{connect_client, init_logging};
use crate::state::AppState;

mod client;
mod error;
mod routes;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();

    let rest_addr = env::var("ANTD_REST_ADDR").unwrap_or_else(|_| "0.0.0.0:8082".to_string());
    let bind_addr: SocketAddr = rest_addr.parse()?;
    let network = env::var("ANTD_NETWORK").unwrap_or_else(|_| "default".to_string());
    let client = Arc::new(connect_client().await?);

    let state = AppState { client, network };
    let app = routes::router(state).layer(CorsLayer::permissive());

    info!("Autonomi 2.0 compatibility gateway listening on {bind_addr}");
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
