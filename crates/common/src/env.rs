use std::{env, fs, time::Duration};

pub fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn secret_env(name: &str, file_name: &str) -> anyhow::Result<Option<String>> {
    if let Some(path) = non_empty_env(file_name) {
        let value = fs::read_to_string(&path)
            .map_err(|err| anyhow::anyhow!("could not read {file_name} at {path}: {err}"))?
            .trim()
            .to_string();
        if !value.is_empty() {
            return Ok(Some(value));
        }
    }
    Ok(non_empty_env(name))
}

/// Parse an environment variable with [`std::str::FromStr`], falling back to
/// `default` when the variable is unset or empty. A present-but-invalid value
/// is an error so misconfiguration fails startup loudly instead of being
/// silently replaced by a default.
pub fn parse_env<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match non_empty_env(name) {
        Some(value) => value
            .parse::<T>()
            .map_err(|err| anyhow::anyhow!("invalid {name} '{value}': {err}")),
        None => Ok(default),
    }
}

/// Like [`parse_env`], but additionally rejects the type's default value
/// (zero for the integer types this is used with).
pub fn parse_nonzero_env<T>(name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr + PartialEq + Default,
    T::Err: std::fmt::Display,
{
    let value = parse_env(name, default)?;
    if value == T::default() {
        anyhow::bail!("{name} must be greater than zero");
    }
    Ok(value)
}

pub fn duration_secs_from_env(name: &str, default: Duration) -> anyhow::Result<Duration> {
    let seconds = parse_env(name, default.as_secs_f64())?;
    if !seconds.is_finite() || seconds < 0.0 {
        anyhow::bail!("invalid {name}: must be a non-negative number of seconds");
    }
    Ok(Duration::from_secs_f64(seconds))
}

pub fn bool_from_env(name: &str, default: bool) -> anyhow::Result<bool> {
    match non_empty_env(name) {
        Some(value) => match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            other => anyhow::bail!("invalid {name} '{other}': expected a boolean"),
        },
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    // Env-var mutation is process-global; run these serially in one test.
    #[test]
    fn parse_env_defaults_and_errors() {
        std::env::remove_var("AUTVID_COMMON_ENV_TEST");
        assert_eq!(parse_env("AUTVID_COMMON_ENV_TEST", 7_usize).unwrap(), 7);

        std::env::set_var("AUTVID_COMMON_ENV_TEST", "42");
        assert_eq!(parse_env("AUTVID_COMMON_ENV_TEST", 7_usize).unwrap(), 42);

        std::env::set_var("AUTVID_COMMON_ENV_TEST", "not-a-number");
        let err = parse_env("AUTVID_COMMON_ENV_TEST", 7_usize).unwrap_err();
        assert!(err.to_string().contains("AUTVID_COMMON_ENV_TEST"));

        std::env::set_var("AUTVID_COMMON_ENV_TEST", "2.5");
        assert_eq!(
            duration_secs_from_env("AUTVID_COMMON_ENV_TEST", Duration::from_secs(1)).unwrap(),
            Duration::from_secs_f64(2.5)
        );

        std::env::set_var("AUTVID_COMMON_ENV_TEST", "-1");
        assert!(duration_secs_from_env("AUTVID_COMMON_ENV_TEST", Duration::from_secs(1)).is_err());

        std::env::set_var("AUTVID_COMMON_ENV_TEST", "true");
        assert!(bool_from_env("AUTVID_COMMON_ENV_TEST", false).unwrap());
        std::env::set_var("AUTVID_COMMON_ENV_TEST", "off");
        assert!(!bool_from_env("AUTVID_COMMON_ENV_TEST", true).unwrap());
        std::env::set_var("AUTVID_COMMON_ENV_TEST", "maybe");
        assert!(bool_from_env("AUTVID_COMMON_ENV_TEST", true).is_err());

        std::env::remove_var("AUTVID_COMMON_ENV_TEST");
    }
}
