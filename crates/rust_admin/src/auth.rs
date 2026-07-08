//! Admin authentication: login/refresh handlers, JWT + refresh tokens,
//! session persistence, request extraction, and cookie construction.
mod cookies;
mod extract;
mod handlers;
mod sessions;
mod tokens;

pub(crate) use cookies::*;
pub(crate) use extract::*;
pub(crate) use handlers::*;
pub(crate) use sessions::*;
pub(crate) use tokens::*;

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

#[cfg(test)]
mod tests;
