use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use chrono::{DateTime, Utc};

use crate::{config::AuthCookieSameSite, errors::ApiError, AppState};

use super::*;

pub(crate) fn auth_success_headers(
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

pub(crate) fn expired_auth_cookie_headers(state: &AppState) -> Result<HeaderMap, ApiError> {
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

pub(crate) fn cookie_max_age_seconds(expires_at: DateTime<Utc>) -> i64 {
    expires_at
        .signed_duration_since(Utc::now())
        .num_seconds()
        .max(0)
}

pub(crate) fn auth_cookie_header(
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

pub(crate) fn csrf_cookie_header(
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

pub(crate) fn expired_auth_cookie_header(
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

pub(crate) fn expired_plain_cookie_header(
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
