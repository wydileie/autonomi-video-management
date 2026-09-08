use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
    network: String,
    peer_count: u32,
    read_ready: bool,
    write_ready: bool,
    routing_table_size: u32,
    protocol_version: &'static str,
}

pub(super) async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let health = state.client.network_health().await;
    let read_ready = health.connected_peers > 0 || health.routing_table_size > 0;
    Json(HealthResponse {
        status: if read_ready { "ok" } else { "degraded" },
        network: state.network,
        peer_count: health.connected_peers,
        read_ready,
        write_ready: health.write_ready && state.payments.can_write().await,
        routing_table_size: health.routing_table_size,
        protocol_version: "autvid-gateway-v2",
    })
}
