use std::env;

use autvid_common::{bool_from_env, constant_time_eq};

use super::*;

pub(crate) fn parse_cookie_same_site_env(
    name: &str,
    default_value: AuthCookieSameSite,
) -> anyhow::Result<AuthCookieSameSite> {
    let raw = env::var(name).unwrap_or_else(|_| default_value.as_cookie_value().to_string());
    AuthCookieSameSite::parse(&raw)
        .ok_or_else(|| anyhow::anyhow!("{name} must be one of Strict, Lax, or None"))
}

pub(crate) fn is_production_environment() -> anyhow::Result<bool> {
    Ok(bool_from_env("AUTVID_STRICT_AUTH", false)?
        || ["APP_ENV", "ENVIRONMENT"].iter().any(|name| {
            matches!(
                env::var(name)
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
                    .as_str(),
                "prod" | "production"
            )
        }))
}

pub(crate) fn is_unsafe_admin_auth_value(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "admin"
            | "administrator"
            | "changeme"
            | "change-me"
            | "change_me"
            | "default"
            | "password"
            | "please-change-me"
            | "replace-me"
            | "secret"
            | "test"
            | "test-secret"
    ) || [
        "change-me",
        "change_me",
        "changeme",
        "change-this",
        "change_this",
        "changethis",
        "replace-me",
        "replace_me",
        "replace-this",
        "replace_this",
    ]
    .iter()
    .any(|placeholder| normalized.contains(placeholder))
}

pub(crate) fn validate_admin_auth_config(
    username: &str,
    password: &str,
    secret: &str,
    ttl_hours: i64,
) -> anyhow::Result<()> {
    if ttl_hours <= 0 {
        anyhow::bail!("ADMIN_AUTH_TTL_HOURS must be greater than zero");
    }
    if !is_production_environment()? {
        return Ok(());
    }

    let mut unsafe_fields = Vec::new();
    if is_unsafe_admin_auth_value(username) {
        unsafe_fields.push("ADMIN_USERNAME");
    }
    if is_unsafe_admin_auth_value(password) {
        unsafe_fields.push("ADMIN_PASSWORD");
    }
    if is_unsafe_admin_auth_value(secret) {
        unsafe_fields.push("ADMIN_AUTH_SECRET");
    }
    if !unsafe_fields.is_empty() {
        anyhow::bail!(
            "Unsafe admin auth configuration for production: {} must not use default, weak, or change-me values",
            unsafe_fields.join(", ")
        );
    }
    if constant_time_eq(secret, password) {
        anyhow::bail!(
            "Unsafe admin auth configuration for production: ADMIN_AUTH_SECRET must not equal ADMIN_PASSWORD"
        );
    }
    if password.len() < 12 {
        anyhow::bail!(
            "Unsafe admin auth configuration for production: ADMIN_PASSWORD must be at least 12 characters"
        );
    }
    if secret.len() < 32 {
        anyhow::bail!(
            "Unsafe admin auth configuration for production: ADMIN_AUTH_SECRET must be at least 32 characters"
        );
    }
    Ok(())
}
