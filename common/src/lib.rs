use std::{
    fmt::Write as _,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use http::HeaderValue;

#[derive(Default)]
pub struct HttpMetrics {
    request_total: AtomicU64,
    request_error_total: AtomicU64,
    request_latency_ms_total: AtomicU64,
}

impl HttpMetrics {
    pub fn record_request(&self, status: u16, latency: Duration) {
        self.request_total.fetch_add(1, Ordering::Relaxed);
        self.request_latency_ms_total.fetch_add(
            latency.as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        if status >= 500 {
            self.request_error_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn render_prometheus(&self, service: &str) -> String {
        let mut output = String::new();
        push_counter(
            &mut output,
            "autvid_http_requests_total",
            "Total HTTP requests handled by service.",
            service,
            self.request_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_http_request_errors_total",
            "Total HTTP requests with 5xx responses.",
            service,
            self.request_error_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_http_request_latency_ms_total",
            "Cumulative HTTP response latency in milliseconds.",
            service,
            self.request_latency_ms_total.load(Ordering::Relaxed),
        );
        output
    }
}

pub fn push_counter(output: &mut String, name: &str, help: &str, service: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} counter");
    let _ = writeln!(output, "{name}{{service=\"{service}\"}} {value}");
}

pub fn push_gauge(output: &mut String, name: &str, help: &str, service: &str, value: u64) {
    let _ = writeln!(output, "# HELP {name} {help}");
    let _ = writeln!(output, "# TYPE {name} gauge");
    let _ = writeln!(output, "{name}{{service=\"{service}\"}} {value}");
}

pub fn normalize_cors_origin(origin: &str) -> anyhow::Result<String> {
    let origin = origin.trim().trim_end_matches('/');
    if origin == "*" {
        anyhow::bail!("CORS_ALLOWED_ORIGINS must list explicit origins, not '*'.");
    }

    let host = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .ok_or_else(|| {
            anyhow::anyhow!("CORS_ALLOWED_ORIGINS entries must start with http:// or https://")
        })?;

    if host.is_empty() || host.contains('/') || host.contains('?') || host.contains('#') {
        anyhow::bail!(
            "CORS_ALLOWED_ORIGINS entries must be origins like 'https://example.com' with no path, query, or wildcard."
        );
    }

    Ok(origin.to_string())
}

pub fn parse_cors_allowed_origins(raw_origins: &str) -> anyhow::Result<Vec<HeaderValue>> {
    raw_origins
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .map(|origin| {
            let origin = normalize_cors_origin(origin)?;
            HeaderValue::from_str(&origin)
                .map_err(|err| anyhow::anyhow!("invalid CORS origin '{}': {}", origin, err))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_origin_normalization_accepts_explicit_origins() {
        assert_eq!(
            normalize_cors_origin(" https://example.com/ ").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_cors_origin("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
    }

    #[test]
    fn cors_origin_normalization_rejects_wildcards_paths_and_missing_schemes() {
        assert!(normalize_cors_origin("*").is_err());
        assert!(normalize_cors_origin("https://example.com/app").is_err());
        assert!(normalize_cors_origin("example.com").is_err());
    }

    #[test]
    fn parses_comma_separated_allowed_origins() {
        let origins = parse_cors_allowed_origins("http://localhost, https://example.com/").unwrap();
        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0], "http://localhost");
        assert_eq!(origins[1], "https://example.com");
    }

    #[test]
    fn renders_http_metrics_as_prometheus_text() {
        let metrics = HttpMetrics::default();
        metrics.record_request(200, std::time::Duration::from_millis(25));
        metrics.record_request(503, std::time::Duration::from_millis(75));

        let rendered = metrics.render_prometheus("test_service");
        assert!(rendered.contains("autvid_http_requests_total{service=\"test_service\"} 2"));
        assert!(rendered.contains("autvid_http_request_errors_total{service=\"test_service\"} 1"));
        assert!(
            rendered.contains("autvid_http_request_latency_ms_total{service=\"test_service\"} 100")
        );
    }
}
