use serde::Deserialize;

#[derive(Deserialize)]
pub struct AntdHealthResponse {
    pub status: String,
    pub network: Option<String>,
}

#[derive(Deserialize)]
pub struct AntdPublicDataResponse {
    pub data: String,
}

#[derive(Debug, Deserialize)]
pub struct AntdWalletAddressResponse {
    pub address: String,
}

#[derive(Debug, Deserialize)]
pub struct AntdWalletBalanceResponse {
    pub balance: String,
    pub gas_balance: String,
}

#[derive(Debug, Deserialize)]
pub struct AntdWalletApproveResponse {
    pub approved: bool,
}

#[derive(Deserialize)]
pub struct AntdDataCostResponse {
    pub cost: Option<String>,
    pub chunk_count: Option<i64>,
    pub estimated_gas_cost_wei: Option<String>,
    pub payment_mode: Option<String>,
}

#[derive(Deserialize)]
pub struct AntdDataPutResponse {
    pub address: String,
    pub cost: Option<String>,
}

#[derive(Deserialize)]
pub struct AntdFilePutResponse {
    pub address: String,
    pub byte_size: u64,
    pub storage_cost_atto: String,
    pub payment_mode_used: String,
}
