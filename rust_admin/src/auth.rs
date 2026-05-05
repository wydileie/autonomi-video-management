use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    Json,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::{config::constant_time_eq, errors::ApiError, AppState};

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

#[derive(Deserialize)]
pub(crate) struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
pub(crate) struct AuthTokenOut {
    access_token: String,
    token_type: &'static str,
    expires_at: String,
    username: String,
}

#[derive(Serialize)]
pub(crate) struct AdminMeOut {
    username: String,
}

pub(crate) async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<AuthTokenOut>, ApiError> {
    if !constant_time_eq(&request.username, &state.config.admin_username)
        || !constant_time_eq(&request.password, &state.config.admin_password)
    {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "Invalid username or password",
        ));
    }

    let expires_at = Utc::now() + Duration::hours(state.config.admin_auth_ttl_hours);
    let token = encode(
        &Header::new(Algorithm::HS256),
        &Claims {
            sub: state.config.admin_username.clone(),
            exp: expires_at.timestamp() as usize,
        },
        &EncodingKey::from_secret(state.config.admin_auth_secret.as_bytes()),
    )
    .map_err(|err| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))?;

    Ok(Json(AuthTokenOut {
        access_token: token,
        token_type: "bearer",
        expires_at: expires_at.to_rfc3339(),
        username: state.config.admin_username.clone(),
    }))
}

pub(crate) async fn auth_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminMeOut>, ApiError> {
    let username = require_admin(&state, &headers)?;
    Ok(Json(AdminMeOut { username }))
}

pub(crate) fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("Login required"))?;
    let token = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| ApiError::unauthorized("Login required"))?;

    let claims = decode::<Claims>(
        token,
        &DecodingKey::from_secret(state.config.admin_auth_secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .map_err(|_| ApiError::unauthorized("Invalid or expired login"))?
    .claims;

    if claims.sub != state.config.admin_username {
        return Err(ApiError::unauthorized("Invalid or expired login"));
    }
    Ok(claims.sub)
}
