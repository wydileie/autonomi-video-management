use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    Json,
};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::{config::constant_time_eq, errors::ApiError, AppState};

const ADMIN_AUTH_COOKIE: &str = "autvid_admin";

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
) -> Result<(HeaderMap, Json<AuthTokenOut>), ApiError> {
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

    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        auth_cookie_header(
            &token,
            state.config.admin_auth_ttl_hours.saturating_mul(3600),
            state.config.admin_auth_cookie_secure,
        )?,
    );

    Ok((
        headers,
        Json(AuthTokenOut {
            access_token: token,
            token_type: "bearer",
            expires_at: expires_at.to_rfc3339(),
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
) -> Result<(HeaderMap, StatusCode), ApiError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        expired_auth_cookie_header(state.config.admin_auth_cookie_secure)?,
    );
    Ok((headers, StatusCode::NO_CONTENT))
}

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

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    auth.strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| {
            if name.trim() == ADMIN_AUTH_COOKIE {
                Some(value.trim())
            } else {
                None
            }
        })
        .filter(|token| !token.is_empty())
}

fn auth_cookie_header(
    token: &str,
    max_age_seconds: i64,
    secure: bool,
) -> Result<HeaderValue, ApiError> {
    let secure_attribute = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{ADMIN_AUTH_COOKIE}={token}; HttpOnly; SameSite=Lax; Path=/api; Max-Age={max_age_seconds}{secure_attribute}"
    ))
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not create admin auth cookie: {err}"),
        )
    })
}

fn expired_auth_cookie_header(secure: bool) -> Result<HeaderValue, ApiError> {
    let secure_attribute = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{ADMIN_AUTH_COOKIE}=; HttpOnly; SameSite=Lax; Path=/api; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT{secure_attribute}"
    ))
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not clear admin auth cookie: {err}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bearer_token_before_cookie_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer bearer-token"),
        );
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("other=value; autvid_admin=cookie-token"),
        );

        assert_eq!(bearer_token(&headers), Some("bearer-token"));
        assert_eq!(cookie_token(&headers), Some("cookie-token"));
        assert_eq!(
            bearer_token(&headers).or_else(|| cookie_token(&headers)),
            Some("bearer-token")
        );
    }

    #[test]
    fn auth_cookie_sets_httponly_samesite_and_optional_secure() {
        let plain = auth_cookie_header("token", 3600, false).unwrap();
        assert_eq!(
            plain.to_str().unwrap(),
            "autvid_admin=token; HttpOnly; SameSite=Lax; Path=/api; Max-Age=3600"
        );

        let secure = auth_cookie_header("token", 3600, true).unwrap();
        assert!(secure.to_str().unwrap().ends_with("; Secure"));
    }

    #[test]
    fn expired_cookie_clears_admin_cookie() {
        let expired = expired_auth_cookie_header(false).unwrap();
        let value = expired.to_str().unwrap();
        assert!(value.contains("autvid_admin="));
        assert!(value.contains("Max-Age=0"));
    }
}
