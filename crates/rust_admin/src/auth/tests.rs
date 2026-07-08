#![allow(clippy::unwrap_used)]
use axum::http::{header, HeaderMap, HeaderValue};

use crate::config::AuthCookieSameSite;

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
