use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
    network: String,
    peer_count: usize,
}

pub(super) async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    let peer_count = state.client.network().connected_peers().await.len();
    Json(HealthResponse {
        status: "ok",
        network: state.network,
        peer_count,
    })
}
