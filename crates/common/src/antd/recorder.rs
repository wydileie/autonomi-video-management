use std::time::Duration;

/// Hook for service-specific metrics on antd client activity. All methods
/// default to no-ops so consumers only implement what they track.
pub trait AntdMetricsRecorder: Send + Sync {
    fn record_request(&self, path: &str, latency: Duration, ok: bool) {
        let _ = (path, latency, ok);
    }

    fn record_upload_retry(&self) {}
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopRecorder;

impl AntdMetricsRecorder for NoopRecorder {}
