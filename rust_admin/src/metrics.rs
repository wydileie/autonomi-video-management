use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use autvid_common::{push_counter, HttpMetrics};

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

    pub(crate) fn record_ffmpeg_duration(&self, duration: Duration) {
        self.ffmpeg_runs_total.fetch_add(1, Ordering::Relaxed);
        self.ffmpeg_duration_ms_total.fetch_add(
            duration.as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
    }

    pub(crate) fn record_antd_request(&self, duration: Duration, ok: bool) {
        self.antd_requests_total.fetch_add(1, Ordering::Relaxed);
        self.antd_request_latency_ms_total.fetch_add(
            duration.as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        if !ok {
            self.antd_request_errors_total
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn record_upload_retry(&self) {
        self.upload_retries_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn render_prometheus(&self) -> String {
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
            "Total Autonomi upload retries scheduled by rust_admin.",
            service,
            self.upload_retries_total.load(Ordering::Relaxed),
        );
        output
    }
}
