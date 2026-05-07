use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::{
    config::{constant_time_eq, AuthCookieSameSite},
    db::db_error,
    errors::ApiError,
    AppState,
};

const ADMIN_AUTH_COOKIE: &str = "autvid_admin";
const ADMIN_REFRESH_COOKIE: &str = "autvid_admin_refresh";
pub(crate) const ADMIN_CSRF_COOKIE: &str = "autvid_csrf";
pub(crate) const ADMIN_CSRF_HEADER: &str = "x-csrf-token";
const ADMIN_AUTH_COOKIE_PATH: &str = "/api";
const ADMIN_REFRESH_COOKIE_PATH: &str = "/api/auth";
// The SPA reads this double-submit value from document.cookie on pages such as /manage.
const ADMIN_CSRF_COOKIE_PATH: &str = "/";
const REFRESH_TOKEN_BYTES: usize = 32;
const CSRF_TOKEN_BYTES: usize = 32;

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    iat: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    jti: Option<String>,
}

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

struct IssuedAccessToken {
    token: String,
    expires_at: DateTime<Utc>,
}

struct IssuedRefreshToken {
    session_id: Uuid,
    token: String,
    token_hash: String,
    expires_at: DateTime<Utc>,
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

fn issue_access_token(state: &AppState) -> Result<IssuedAccessToken, ApiError> {
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

async fn create_refresh_session(
    state: &AppState,
    username: &str,
) -> Result<IssuedRefreshToken, ApiError> {
    let refresh = new_refresh_token(state);
    sqlx::query(
        r#"
        INSERT INTO admin_refresh_sessions (id, username, token_hash, expires_at)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(refresh.session_id)
    .bind(username)
    .bind(&refresh.token_hash)
    .bind(refresh.expires_at)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(refresh)
}

async fn rotate_refresh_session(
    state: &AppState,
    token: &str,
) -> Result<IssuedRefreshToken, ApiError> {
    let token_hash = refresh_token_hash(token);
    let refresh = new_refresh_token(state);
    let mut tx = state.pool.begin().await.map_err(db_error)?;
    let row = sqlx::query(
        r#"
        SELECT id, username
        FROM admin_refresh_sessions
        WHERE token_hash=$1
            AND revoked_at IS NULL
            AND expires_at > NOW()
        FOR UPDATE
        "#,
    )
    .bind(&token_hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?;

    let Some(row) = row else {
        return Err(ApiError::unauthorized("Invalid or expired refresh token"));
    };
    let previous_session_id = row.try_get::<Uuid, _>("id").map_err(db_error)?;
    let username = row.try_get::<String, _>("username").map_err(db_error)?;
    if username != state.config.admin_username {
        return Err(ApiError::unauthorized("Invalid or expired refresh token"));
    }

    sqlx::query(
        r#"
        INSERT INTO admin_refresh_sessions (id, username, token_hash, expires_at)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(refresh.session_id)
    .bind(&username)
    .bind(&refresh.token_hash)
    .bind(refresh.expires_at)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    sqlx::query(
        r#"
        UPDATE admin_refresh_sessions
        SET revoked_at=NOW(),
            last_used_at=NOW(),
            replaced_by_session_id=$2
        WHERE id=$1
        "#,
    )
    .bind(previous_session_id)
    .bind(refresh.session_id)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(refresh)
}

async fn revoke_refresh_session(state: &AppState, token: &str) -> Result<(), ApiError> {
    let token_hash = refresh_token_hash(token);
    sqlx::query(
        r#"
        UPDATE admin_refresh_sessions
        SET revoked_at=COALESCE(revoked_at, NOW()),
            last_used_at=NOW()
        WHERE token_hash=$1
            AND revoked_at IS NULL
        "#,
    )
    .bind(&token_hash)
    .execute(&state.pool)
    .await
    .map_err(db_error)?;
    Ok(())
}

fn new_refresh_token(state: &AppState) -> IssuedRefreshToken {
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

fn generate_refresh_token() -> String {
    let mut bytes = [0u8; REFRESH_TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn refresh_token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex_lower(&hasher.finalize())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
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
    access_cookie_token(headers)
}

fn access_cookie_token(headers: &HeaderMap) -> Option<&str> {
    named_cookie_token(headers, ADMIN_AUTH_COOKIE)
}

fn refresh_cookie_token(headers: &HeaderMap) -> Option<&str> {
    named_cookie_token(headers, ADMIN_REFRESH_COOKIE)
}

fn named_cookie_token<'a>(headers: &'a HeaderMap, cookie_name: &str) -> Option<&'a str> {
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

fn auth_success_headers(
    state: &AppState,
    access: &IssuedAccessToken,
    refresh: &IssuedRefreshToken,
) -> Result<HeaderMap, ApiError> {
    let same_site = state.config.admin_auth_cookie_same_site();
    let csrf_token = generate_csrf_token();
    let mut headers = HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        auth_cookie_header(
            ADMIN_AUTH_COOKIE,
            &access.token,
            ADMIN_AUTH_COOKIE_PATH,
            cookie_max_age_seconds(access.expires_at),
            state.config.admin_auth_cookie_secure,
            same_site,
        )?,
    );
    headers.append(
        header::SET_COOKIE,
        auth_cookie_header(
            ADMIN_REFRESH_COOKIE,
            &refresh.token,
            ADMIN_REFRESH_COOKIE_PATH,
            cookie_max_age_seconds(refresh.expires_at),
            state.config.admin_auth_cookie_secure,
            same_site,
        )?,
    );
    headers.append(
        header::SET_COOKIE,
        csrf_cookie_header(
            &csrf_token,
            ADMIN_CSRF_COOKIE_PATH,
            cookie_max_age_seconds(refresh.expires_at),
            state.config.admin_auth_cookie_secure,
            same_site,
        )?,
    );
    Ok(headers)
}

fn expired_auth_cookie_headers(state: &AppState) -> Result<HeaderMap, ApiError> {
    let same_site = state.config.admin_auth_cookie_same_site();
    let mut headers = HeaderMap::new();
    headers.append(
        header::SET_COOKIE,
        expired_auth_cookie_header(
            ADMIN_AUTH_COOKIE,
            ADMIN_AUTH_COOKIE_PATH,
            state.config.admin_auth_cookie_secure,
            same_site,
        )?,
    );
    headers.append(
        header::SET_COOKIE,
        expired_auth_cookie_header(
            ADMIN_REFRESH_COOKIE,
            ADMIN_REFRESH_COOKIE_PATH,
            state.config.admin_auth_cookie_secure,
            same_site,
        )?,
    );
    headers.append(
        header::SET_COOKIE,
        expired_plain_cookie_header(
            ADMIN_CSRF_COOKIE,
            ADMIN_CSRF_COOKIE_PATH,
            state.config.admin_auth_cookie_secure,
            same_site,
        )?,
    );
    Ok(headers)
}

fn cookie_max_age_seconds(expires_at: DateTime<Utc>) -> i64 {
    expires_at
        .signed_duration_since(Utc::now())
        .num_seconds()
        .max(0)
}

fn auth_cookie_header(
    cookie_name: &str,
    token: &str,
    path: &str,
    max_age_seconds: i64,
    secure: bool,
    same_site: AuthCookieSameSite,
) -> Result<HeaderValue, ApiError> {
    let secure_attribute = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{cookie_name}={token}; HttpOnly; SameSite={}; Path={path}; Max-Age={max_age_seconds}{secure_attribute}",
        same_site.as_cookie_value()
    ))
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not create {cookie_name} cookie: {err}"),
        )
    })
}

fn csrf_cookie_header(
    token: &str,
    path: &str,
    max_age_seconds: i64,
    secure: bool,
    same_site: AuthCookieSameSite,
) -> Result<HeaderValue, ApiError> {
    let secure_attribute = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{ADMIN_CSRF_COOKIE}={token}; SameSite={}; Path={path}; Max-Age={max_age_seconds}{secure_attribute}",
        same_site.as_cookie_value()
    ))
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not create {ADMIN_CSRF_COOKIE} cookie: {err}"),
        )
    })
}

fn expired_auth_cookie_header(
    cookie_name: &str,
    path: &str,
    secure: bool,
    same_site: AuthCookieSameSite,
) -> Result<HeaderValue, ApiError> {
    let secure_attribute = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{cookie_name}=; HttpOnly; SameSite={}; Path={path}; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT{secure_attribute}",
        same_site.as_cookie_value()
    ))
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not clear {cookie_name} cookie: {err}"),
        )
    })
}

fn expired_plain_cookie_header(
    cookie_name: &str,
    path: &str,
    secure: bool,
    same_site: AuthCookieSameSite,
) -> Result<HeaderValue, ApiError> {
    let secure_attribute = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{cookie_name}=; SameSite={}; Path={path}; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT{secure_attribute}",
        same_site.as_cookie_value()
    ))
    .map_err(|err| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Could not clear {cookie_name} cookie: {err}"),
        )
    })
}

fn generate_csrf_token() -> String {
    let mut bytes = [0u8; CSRF_TOKEN_BYTES];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
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
            HeaderValue::from_static(
                "other=value; autvid_admin=cookie-token; autvid_admin_refresh=refresh-token",
            ),
        );

        assert_eq!(bearer_token(&headers), Some("bearer-token"));
        assert_eq!(cookie_token(&headers), Some("cookie-token"));
        assert_eq!(refresh_cookie_token(&headers), Some("refresh-token"));
        assert_eq!(
            bearer_token(&headers).or_else(|| cookie_token(&headers)),
            Some("bearer-token")
        );
    }

    #[test]
    fn auth_cookie_sets_httponly_samesite_and_optional_secure() {
        let plain = auth_cookie_header(
            ADMIN_AUTH_COOKIE,
            "token",
            ADMIN_AUTH_COOKIE_PATH,
            3600,
            false,
            AuthCookieSameSite::Lax,
        )
        .unwrap();
        assert_eq!(
            plain.to_str().unwrap(),
            "autvid_admin=token; HttpOnly; SameSite=Lax; Path=/api; Max-Age=3600"
        );

        let refresh = auth_cookie_header(
            ADMIN_REFRESH_COOKIE,
            "refresh-token",
            ADMIN_REFRESH_COOKIE_PATH,
            7200,
            true,
            AuthCookieSameSite::Strict,
        )
        .unwrap();
        assert_eq!(
            refresh.to_str().unwrap(),
            "autvid_admin_refresh=refresh-token; HttpOnly; SameSite=Strict; Path=/api/auth; Max-Age=7200; Secure"
        );

        let secure = auth_cookie_header(
            ADMIN_AUTH_COOKIE,
            "token",
            ADMIN_AUTH_COOKIE_PATH,
            3600,
            true,
            AuthCookieSameSite::Lax,
        )
        .unwrap();
        assert!(secure.to_str().unwrap().ends_with("; Secure"));
    }

    #[test]
    fn expired_cookie_clears_admin_cookie() {
        let expired = expired_auth_cookie_header(
            ADMIN_AUTH_COOKIE,
            ADMIN_AUTH_COOKIE_PATH,
            false,
            AuthCookieSameSite::Lax,
        )
        .unwrap();
        let value = expired.to_str().unwrap();
        assert!(value.contains("autvid_admin="));
        assert!(value.contains("Max-Age=0"));
    }

    #[test]
    fn refresh_token_hash_is_deterministic_and_not_plaintext() {
        let hash = refresh_token_hash("refresh-token");
        assert_eq!(hash, refresh_token_hash("refresh-token"));
        assert_eq!(hash.len(), 64);
        assert_ne!(hash, "refresh-token");
    }
}
