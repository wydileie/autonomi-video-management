use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Shared HTTP error body for all autvid services.
///
/// Renders as `{"detail": <message>}` plus an optional machine-readable
/// `"code"` field, and sets `WWW-Authenticate: Bearer` for auth challenges.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: Option<&'static str>,
    pub detail: String,
    authenticate: bool,
}

impl ApiError {
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            code: None,
            detail: detail.into(),
            authenticate: false,
        }
    }

    pub fn with_code(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            code: Some(code),
            detail: detail.into(),
            authenticate: false,
        }
    }

    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::with_code(StatusCode::BAD_REQUEST, "BAD_REQUEST", detail)
    }

    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: None,
            detail: detail.into(),
            authenticate: true,
        }
    }

    pub fn wallet(detail: impl Into<String>) -> Self {
        Self::with_code(StatusCode::SERVICE_UNAVAILABLE, "WALLET_ERROR", detail)
    }

    pub fn from_anyhow(err: anyhow::Error) -> Self {
        Self::from_autonomi_message(err.to_string())
    }

    /// Classify an Autonomi client error message into an HTTP status and
    /// machine-readable code (network trouble vs. bad input vs. unknown).
    pub fn from_autonomi_message(message: String) -> Self {
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
            code: Some(code),
            detail: message,
            authenticate: false,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.code {
            Some(code) => json!({ "detail": self.detail, "code": code }),
            None => json!({ "detail": self.detail }),
        };
        let mut response = (self.status, Json(body)).into_response();
        if self.authenticate {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn classifies_autonomi_messages() {
        let err = ApiError::from_autonomi_message("Failed to connect to peers".into());
        assert_eq!(err.status, StatusCode::BAD_GATEWAY);
        assert_eq!(err.code, Some("NETWORK_ERROR"));

        let err = ApiError::from_autonomi_message("InvalidData in payload".into());
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert_eq!(err.code, Some("BAD_REQUEST"));

        let err = ApiError::from_autonomi_message("something else".into());
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code, Some("AUTONOMI_ERROR"));
    }

    #[tokio::test]
    async fn renders_detail_body_and_auth_challenge() {
        let response = ApiError::unauthorized("no token").into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE).unwrap(),
            "Bearer"
        );
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({ "detail": "no token" }));

        let response = ApiError::wallet("locked").into_response();
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, json!({ "detail": "locked", "code": "WALLET_ERROR" }));
    }
}
