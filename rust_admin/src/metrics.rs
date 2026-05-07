use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    sync::Mutex,
    time::Duration,
};

use autvid_common::{
    push_counter, push_gauge, push_histogram, push_histogram_header, push_histogram_samples,
    HttpMetrics, LatencyHistogram,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct JobMetricsSnapshot {
    pub(crate) queued: u64,
    pub(crate) running: u64,
    pub(crate) failed: u64,
    pub(crate) succeeded: u64,
    pub(crate) oldest_queued_age_seconds: u64,
}

pub(crate) struct AdminMetrics {
    pub(crate) http: HttpMetrics,
    jobs_started_total: AtomicU64,
    jobs_succeeded_total: AtomicU64,
    jobs_failed_total: AtomicU64,
    ffmpeg_runs_total: AtomicU64,
    ffmpeg_duration_ms_total: AtomicU64,
    antd_requests_total: AtomicU64,
    antd_request_errors_total: AtomicU64,
    antd_request_latency_ms_total: AtomicU64,
    upload_retries_total: AtomicU64,
    ffmpeg_duration: Mutex<HashMap<String, LatencyHistogram>>,
    antd_request_latency: Mutex<HashMap<String, LatencyHistogram>>,
    job_pickup_latency: LatencyHistogram,
}

impl Default for AdminMetrics {
    fn default() -> Self {
        Self {
            http: HttpMetrics::default(),
            jobs_started_total: AtomicU64::new(0),
            jobs_succeeded_total: AtomicU64::new(0),
            jobs_failed_total: AtomicU64::new(0),
            ffmpeg_runs_total: AtomicU64::new(0),
            ffmpeg_duration_ms_total: AtomicU64::new(0),
            antd_requests_total: AtomicU64::new(0),
            antd_request_errors_total: AtomicU64::new(0),
            antd_request_latency_ms_total: AtomicU64::new(0),
            upload_retries_total: AtomicU64::new(0),
            ffmpeg_duration: Mutex::new(HashMap::new()),
            antd_request_latency: Mutex::new(HashMap::new()),
            job_pickup_latency: LatencyHistogram::default(),
        }
    }
}

impl AdminMetrics {
    pub(crate) fn record_job_started(&self) {
        self.jobs_started_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_job_succeeded(&self) {
        self.jobs_succeeded_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_job_failed(&self) {
        self.jobs_failed_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_ffmpeg_duration(&self, resolution: &str, duration: Duration) {
        self.ffmpeg_runs_total.fetch_add(1, Ordering::Relaxed);
        self.ffmpeg_duration_ms_total.fetch_add(
            duration.as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        self.ffmpeg_duration
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .entry(resolution.to_string())
            .or_default()
            .record_duration(duration);
    }

    pub(crate) fn record_antd_request(&self, endpoint: &str, duration: Duration, ok: bool) {
        self.antd_requests_total.fetch_add(1, Ordering::Relaxed);
        self.antd_request_latency_ms_total.fetch_add(
            duration.as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        self.antd_request_latency
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .entry(endpoint.to_string())
            .or_default()
            .record_duration(duration);
        if !ok {
            self.antd_request_errors_total
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn record_job_pickup_latency(&self, duration: Duration) {
        self.job_pickup_latency.record_duration(duration);
    }

    pub(crate) fn record_upload_retry(&self) {
        self.upload_retries_total.fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn render_prometheus(&self) -> String {
        self.render_prometheus_with_jobs(None)
    }

    pub(crate) fn render_prometheus_with_jobs(&self, jobs: Option<JobMetricsSnapshot>) -> String {
        let service = "rust_admin";
        let mut output = self.http.render_prometheus(service);
        push_counter(
            &mut output,
            "autvid_admin_jobs_started_total",
            "Total durable admin jobs started by workers.",
            service,
            self.jobs_started_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_admin_jobs_succeeded_total",
            "Total durable admin jobs completed successfully.",
            service,
            self.jobs_succeeded_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_admin_jobs_failed_total",
            "Total durable admin job attempts that returned an error.",
            service,
            self.jobs_failed_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_admin_ffmpeg_runs_total",
            "Total FFmpeg rendition runs.",
            service,
            self.ffmpeg_runs_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_admin_ffmpeg_duration_ms_total",
            "Cumulative FFmpeg rendition runtime in milliseconds.",
            service,
            self.ffmpeg_duration_ms_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_admin_antd_requests_total",
            "Total outbound requests from rust_admin to antd.",
            service,
            self.antd_requests_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_admin_antd_request_errors_total",
            "Total outbound antd requests that failed before returning usable data.",
            service,
            self.antd_request_errors_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_admin_antd_request_latency_ms_total",
            "Cumulative outbound antd request latency in milliseconds.",
            service,
            self.antd_request_latency_ms_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut output,
            "autvid_admin_upload_retries_total",
            "Total retry attempts scheduled for Autonomi uploads and cost quotes by rust_admin.",
            service,
            self.upload_retries_total.load(Ordering::Relaxed),
        );
        {
            let ffmpeg_duration = self
                .ffmpeg_duration
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            if !ffmpeg_duration.is_empty() {
                push_histogram_header(
                    &mut output,
                    "autvid_admin_ffmpeg_duration_ms",
                    "FFmpeg rendition runtime in milliseconds.",
                );
            }
            for (resolution, histogram) in ffmpeg_duration.iter() {
                let snapshot = histogram.snapshot();
                push_histogram_samples(
                    &mut output,
                    "autvid_admin_ffmpeg_duration_ms",
                    service,
                    &[("resolution", resolution.as_str())],
                    &snapshot,
                );
            }
        }
        {
            let antd_request_latency = self
                .antd_request_latency
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            if !antd_request_latency.is_empty() {
                push_histogram_header(
                    &mut output,
                    "autvid_admin_antd_request_latency_ms",
                    "Outbound antd request latency in milliseconds.",
                );
            }
            for (endpoint, histogram) in antd_request_latency.iter() {
                let snapshot = histogram.snapshot();
                push_histogram_samples(
                    &mut output,
                    "autvid_admin_antd_request_latency_ms",
                    service,
                    &[("endpoint", endpoint.as_str())],
                    &snapshot,
                );
            }
        }
        let job_pickup_snapshot = self.job_pickup_latency.snapshot();
        push_histogram(
            &mut output,
            "autvid_admin_job_pickup_latency_ms",
            "Time queued jobs waited before a worker picked them up, in milliseconds.",
            service,
            &[],
            &job_pickup_snapshot,
        );
        if let Some(jobs) = jobs {
            push_gauge(
                &mut output,
                "autvid_admin_jobs_queued",
                "Current queued durable admin jobs.",
                service,
                jobs.queued,
            );
            push_gauge(
                &mut output,
                "autvid_admin_jobs_running",
                "Current running durable admin jobs.",
                service,
                jobs.running,
            );
            push_gauge(
                &mut output,
                "autvid_admin_jobs_failed",
                "Current failed durable admin jobs.",
                service,
                jobs.failed,
            );
            push_gauge(
                &mut output,
                "autvid_admin_jobs_succeeded",
                "Current succeeded durable admin jobs.",
                service,
                jobs.succeeded,
            );
            push_gauge(
                &mut output,
                "autvid_admin_oldest_queued_job_age_seconds",
                "Age in seconds of the oldest queued durable admin job.",
                service,
                jobs.oldest_queued_age_seconds,
            );
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_admin_metrics_as_prometheus_text() {
        let metrics = AdminMetrics::default();
        metrics.http.record_request(200, Duration::from_millis(10));
        metrics.record_job_started();
        metrics.record_job_succeeded();
        metrics.record_job_failed();
        metrics.record_ffmpeg_duration("720p", Duration::from_millis(1500));
        metrics.record_antd_request("/v1/file/public", Duration::from_millis(80), true);
        metrics.record_antd_request("/v1/data/cost", Duration::from_millis(20), false);
        metrics.record_job_pickup_latency(Duration::from_millis(12));
        metrics.record_upload_retry();

        let rendered = metrics.render_prometheus();

        assert!(rendered.contains("autvid_http_requests_total{service=\"rust_admin\"} 1"));
        assert!(rendered.contains("autvid_admin_jobs_started_total{service=\"rust_admin\"} 1"));
        assert!(rendered.contains("autvid_admin_jobs_succeeded_total{service=\"rust_admin\"} 1"));
        assert!(rendered.contains("autvid_admin_jobs_failed_total{service=\"rust_admin\"} 1"));
        assert!(rendered.contains("autvid_admin_ffmpeg_runs_total{service=\"rust_admin\"} 1"));
        assert!(
            rendered.contains("autvid_admin_ffmpeg_duration_ms_total{service=\"rust_admin\"} 1500")
        );
        assert!(rendered.contains("autvid_admin_antd_requests_total{service=\"rust_admin\"} 2"));
        assert!(
            rendered.contains("autvid_admin_antd_request_errors_total{service=\"rust_admin\"} 1")
        );
        assert!(rendered
            .contains("autvid_admin_antd_request_latency_ms_total{service=\"rust_admin\"} 100"));
        assert!(rendered.contains("autvid_admin_upload_retries_total{service=\"rust_admin\"} 1"));
        assert!(rendered.contains(
            "autvid_admin_ffmpeg_duration_ms_bucket{service=\"rust_admin\",resolution=\"720p\""
        ));
        assert!(rendered.contains(
            "autvid_admin_antd_request_latency_ms_bucket{service=\"rust_admin\",endpoint=\"/v1/data/cost\""
        ));
        assert!(
            rendered.contains("autvid_admin_job_pickup_latency_ms_bucket{service=\"rust_admin\"")
        );
    }
}
