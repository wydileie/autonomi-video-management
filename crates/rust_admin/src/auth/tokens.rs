use axum::http::StatusCode;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{errors::ApiError, AppState};

use super::*;

#[derive(Serialize, Deserialize)]
pub(crate) struct Claims {
    pub(crate) sub: String,
    pub(crate) exp: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) iat: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) jti: Option<String>,
}

pub(crate) struct IssuedAccessToken {
    pub(crate) token: String,
    pub(crate) expires_at: DateTime<Utc>,
}

pub(crate) struct IssuedRefreshToken {
    pub(crate) session_id: Uuid,
    pub(crate) token: String,
    pub(crate) token_hash: String,
    pub(crate) expires_at: DateTime<Utc>,
}

pub(crate) fn issue_access_token(state: &AppState) -> Result<IssuedAccessToken, ApiError> {
    let issued_at = Utc::now();
    let expires_at = issued_at + Duration::hours(state.config.admin_auth_ttl_hours);
    let token = encode(
        &Header::new(Algorithm::HS256),
        &Claims {
            sub: state.config.admin_username.clone(),
            exp: expires_at.timestamp() as usize,
            iat: Some(issued_at.timestamp() as usize),
            jti: Some(Uuid::new_v4().to_string()),
        },
        &EncodingKey::from_secret(state.config.admin_auth_secret.as_bytes()),
    )
    .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;
    Ok(IssuedAccessToken { token, expires_at })
}

pub(crate) fn new_refresh_token(state: &AppState) -> IssuedRefreshToken {
    let token = generate_refresh_token();
    let token_hash = refresh_token_hash(&token);
    let expires_at = Utc::now() + Duration::hours(state.config.admin_refresh_token_ttl_hours());
    IssuedRefreshToken {
        session_id: Uuid::new_v4(),
        token,
        token_hash,
        expires_at,
    }
}

pub(crate) fn generate_refresh_token() -> String {
    let mut bytes = [0u8; REFRESH_TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn refresh_token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex_lower(&hasher.finalize())
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(crate) fn generate_csrf_token() -> String {
    let mut bytes = [0u8; CSRF_TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
