use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize)]
pub(super) struct WalletAddressResponse {
    address: String,
}

#[derive(Serialize)]
pub(super) struct WalletBalanceResponse {
    balance: String,
    gas_balance: String,
}

#[derive(Serialize)]
pub(super) struct WalletApproveResponse {
    approved: bool,
}

pub(super) async fn wallet_address(
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

pub(super) async fn wallet_balance(
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

pub(super) async fn wallet_approve(
    State(state): State<AppState>,
) -> Result<Json<WalletApproveResponse>, ApiError> {
    state
        .client
        .approve_token_spend()
        .await
        .map_err(|err| ApiError::from_message(err.to_string()))?;
    Ok(Json(WalletApproveResponse { approved: true }))
}
