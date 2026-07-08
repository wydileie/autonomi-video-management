use std::{
    error::Error,
    fmt,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use http::{Method, StatusCode};
use rand::Rng;

#[derive(Debug)]
pub struct AutonomiHttpStatusError {
    pub method: Method,
    pub path: String,
    pub status: StatusCode,
    pub body: String,
}

impl fmt::Display for AutonomiHttpStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} {} failed: {} {}",
            self.method, self.path, self.status, self.body
        )
    }
}

impl Error for AutonomiHttpStatusError {}

#[derive(Debug, Default)]
pub struct CircuitBreaker {
    consecutive_failures: AtomicUsize,
    opened_until_epoch_ms: AtomicU64,
    last_retryable_error: Mutex<Option<String>>,
}

impl CircuitBreaker {
    const FAILURE_THRESHOLD: usize = 5;
    const OPEN_DURATION: Duration = Duration::from_secs(30);

    pub fn check(&self) -> anyhow::Result<()> {
        let now = epoch_millis();
        let opened_until = self.opened_until_epoch_ms.load(Ordering::Relaxed);
        if opened_until > now {
            let remaining_ms = opened_until.saturating_sub(now);
            let last_retryable_error = self
                .last_retryable_error
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .clone();
            if let Some(last_retryable_error) = last_retryable_error {
                anyhow::bail!(
                    "Autonomi request circuit is open for {}ms; last retryable error: {}",
                    remaining_ms,
                    last_retryable_error
                );
            }
            anyhow::bail!("Autonomi request circuit is open for {}ms", remaining_ms);
        }
        Ok(())
    }

    pub fn record_result<T>(&self, result: &anyhow::Result<T>) {
        if result.is_ok() {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            self.opened_until_epoch_ms.store(0, Ordering::Relaxed);
            *self
                .last_retryable_error
                .lock()
                .unwrap_or_else(|err| err.into_inner()) = None;
            return;
        }

        let Some(err) = result.as_ref().err() else {
            return;
        };
        if !is_retryable_antd_error(err) {
            self.consecutive_failures.store(0, Ordering::Relaxed);
            *self
                .last_retryable_error
                .lock()
                .unwrap_or_else(|err| err.into_inner()) = None;
            return;
        }

        *self
            .last_retryable_error
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some(err.to_string());
        let failures = self
            .consecutive_failures
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if failures >= Self::FAILURE_THRESHOLD {
            let opened_until = epoch_millis()
                .saturating_add(Self::OPEN_DURATION.as_millis().min(u128::from(u64::MAX)) as u64);
            self.opened_until_epoch_ms
                .store(opened_until, Ordering::Relaxed);
        }
    }
}

pub fn is_retryable_antd_error(err: &anyhow::Error) -> bool {
    if let Some(status) = err
        .downcast_ref::<AutonomiHttpStatusError>()
        .map(|err| err.status)
    {
        return status == StatusCode::REQUEST_TIMEOUT
            || status == StatusCode::TOO_MANY_REQUESTS
            || status.is_server_error();
    }

    if let Some(err) = err.downcast_ref::<reqwest::Error>() {
        return err.is_connect() || err.is_timeout() || err.is_body();
    }

    false
}

pub fn jitter_duration(base: Duration) -> Duration {
    if base.is_zero() {
        return base;
    }
    let factor = rand::thread_rng().gen_range(0.8..=1.2);
    let millis = (base.as_millis() as f64 * factor).round().max(1.0) as u64;
    Duration::from_millis(millis)
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn retry_classifier_uses_shared_http_status_error() {
        let err: anyhow::Error = AutonomiHttpStatusError {
            method: Method::GET,
            path: "/health".to_string(),
            status: StatusCode::TOO_MANY_REQUESTS,
            body: "slow down".to_string(),
        }
        .into();
        assert!(is_retryable_antd_error(&err));

        let err: anyhow::Error = AutonomiHttpStatusError {
            method: Method::POST,
            path: "/v1/data/cost".to_string(),
            status: StatusCode::REQUEST_TIMEOUT,
            body: String::new(),
        }
        .into();
        assert!(is_retryable_antd_error(&err));

        let err: anyhow::Error = AutonomiHttpStatusError {
            method: Method::GET,
            path: "/missing".to_string(),
            status: StatusCode::NOT_FOUND,
            body: "missing".to_string(),
        }
        .into();
        assert!(!is_retryable_antd_error(&err));
    }

    #[test]
    fn circuit_breaker_opens_after_retryable_failures_and_resets_on_success() {
        let breaker = CircuitBreaker::default();

        for _ in 0..5 {
            let result: anyhow::Result<()> = Err(AutonomiHttpStatusError {
                method: Method::GET,
                path: "/health".to_string(),
                status: StatusCode::SERVICE_UNAVAILABLE,
                body: "unavailable".to_string(),
            }
            .into());
            breaker.record_result(&result);
        }

        assert!(breaker.check().is_err());
        assert!(breaker
            .check()
            .unwrap_err()
            .to_string()
            .contains("last retryable error: GET /health failed"));

        let result: anyhow::Result<()> = Ok(());
        breaker.record_result(&result);
        assert!(breaker.check().is_ok());
    }

    #[test]
    fn jitter_leaves_zero_duration_unchanged() {
        assert_eq!(jitter_duration(Duration::ZERO), Duration::ZERO);
    }
}
