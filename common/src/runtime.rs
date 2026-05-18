use std::{env, fs};

use subtle::ConstantTimeEq;

pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

pub fn constant_time_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_comparison_matches_string_equality() {
        assert!(constant_time_eq("same", "same"));
        assert!(!constant_time_eq("same", "different"));
    }
}
