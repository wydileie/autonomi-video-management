use axum::http::{header, HeaderMap, StatusCode};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

use autvid_common::constant_time_eq;

use crate::{errors::ApiError, AppState};

use super::*;

pub(crate) fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    let token = bearer_token(headers)
        .or_else(|| cookie_token(headers))
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

pub(crate) fn require_csrf(headers: &HeaderMap) -> Result<(), ApiError> {
    let cookie = csrf_cookie_token(headers)
        .ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "CSRF token required"))?;
    let header = csrf_header_token(headers)
        .ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "CSRF token required"))?;
    if !constant_time_eq(cookie, header) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "CSRF token mismatch"));
    }
    Ok(())
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    auth.strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

pub(crate) fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    access_cookie_token(headers)
}

pub(crate) fn access_cookie_token(headers: &HeaderMap) -> Option<&str> {
    named_cookie_token(headers, ADMIN_AUTH_COOKIE)
}

pub(crate) fn refresh_cookie_token(headers: &HeaderMap) -> Option<&str> {
    named_cookie_token(headers, ADMIN_REFRESH_COOKIE)
}

pub(crate) fn named_cookie_token<'a>(headers: &'a HeaderMap, cookie_name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| {
            if name.trim() == cookie_name {
                Some(value.trim())
            } else {
                None
            }
        })
        .filter(|token| !token.is_empty())
}

pub(crate) fn csrf_cookie_token(headers: &HeaderMap) -> Option<&str> {
    named_cookie_token(headers, ADMIN_CSRF_COOKIE)
}

pub(crate) fn csrf_header_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(ADMIN_CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|token| !token.is_empty())
}
