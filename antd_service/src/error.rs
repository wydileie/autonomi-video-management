use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
    code: &'static str,
}

#[derive(Debug)]
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "BAD_REQUEST",
            message: message.into(),
        }
    }

    pub(crate) fn wallet(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "WALLET_ERROR",
            message: message.into(),
        }
    }

    pub(crate) fn from_anyhow(err: anyhow::Error) -> Self {
        Self::from_message(err.to_string())
    }

    pub(crate) fn from_message(message: String) -> Self {
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
