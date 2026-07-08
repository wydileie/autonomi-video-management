pub mod antd;
pub mod env;
pub mod error;
pub mod health;
pub mod metrics;
pub mod resilience;
pub mod security;
pub mod shutdown;

// Flat re-exports preserve the pre-split public API.
pub use env::{
    bool_from_env, duration_secs_from_env, non_empty_env, parse_env, parse_nonzero_env, secret_env,
};
pub use error::ApiError;
pub use health::{run_healthcheck_from_args, run_http_healthcheck};
pub use metrics::{
    push_counter, push_gauge, push_histogram, push_histogram_header, push_histogram_samples,
    HistogramSnapshot, HttpMetrics, LatencyHistogram,
};
pub use resilience::{
    is_retryable_antd_error, jitter_duration, AutonomiHttpStatusError, CircuitBreaker,
};
pub use security::{
    constant_time_eq, cors_allowed_origins_from_env, normalize_cors_origin,
    parse_cors_allowed_origins,
};
pub use shutdown::shutdown_signal;
