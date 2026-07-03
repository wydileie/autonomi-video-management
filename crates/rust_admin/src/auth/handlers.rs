use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};

use autvid_common::constant_time_eq;

use crate::{errors::ApiError, AppState};

use super::*;

#[derive(Deserialize)]
pub(crate) struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
pub(crate) struct AuthTokenOut {
    expires_at: String,
    refresh_token_expires_at: String,
    username: String,
}

#[derive(Serialize)]
pub(crate) struct AdminMeOut {
    username: String,
}

pub(crate) async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<(HeaderMap, Json<AuthTokenOut>), ApiError> {
    if !constant_time_eq(&request.username, &state.config.admin_username)
        || !constant_time_eq(&request.password, &state.config.admin_password)
    {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "Invalid username or password",
        ));
    }

    let access = issue_access_token(&state)?;
    let refresh = create_refresh_session(&state, &state.config.admin_username).await?;
    let headers = auth_success_headers(&state, &access, &refresh)?;

    Ok((
        headers,
        Json(AuthTokenOut {
            expires_at: access.expires_at.to_rfc3339(),
            refresh_token_expires_at: refresh.expires_at.to_rfc3339(),
            username: state.config.admin_username.clone(),
        }),
    ))
}

pub(crate) async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<AuthTokenOut>), ApiError> {
    let refresh_token = refresh_cookie_token(&headers)
        .ok_or_else(|| ApiError::unauthorized("Refresh token required"))?;
    let refresh = rotate_refresh_session(&state, refresh_token).await?;
    let access = issue_access_token(&state)?;
    let headers = auth_success_headers(&state, &access, &refresh)?;

    Ok((
        headers,
        Json(AuthTokenOut {
            expires_at: access.expires_at.to_rfc3339(),
            refresh_token_expires_at: refresh.expires_at.to_rfc3339(),
            username: state.config.admin_username.clone(),
        }),
    ))
}

pub(crate) async fn auth_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AdminMeOut>, ApiError> {
    let username = require_admin(&state, &headers)?;
    Ok(Json(AdminMeOut { username }))
}

pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, StatusCode), ApiError> {
    require_csrf(&headers)?;
    if let Some(refresh_token) = refresh_cookie_token(&headers) {
        revoke_refresh_session(&state, refresh_token).await?;
    }
    Ok((expired_auth_cookie_headers(&state)?, StatusCode::NO_CONTENT))
}
