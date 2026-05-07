use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use sqlx::Row;

use crate::{
    metrics::JobMetricsSnapshot,
    models::{AutonomiHealth, HealthResponse, PostgresHealth},
    state::AppState,
    MIN_ANTD_SELF_ENCRYPTION_BYTES,
};

const JOB_METRICS_CACHE_TTL: Duration = Duration::from_secs(5);
static JOB_METRICS_CACHE: OnceLock<Mutex<Option<CachedJobMetrics>>> = OnceLock::new();

#[derive(Clone, Copy)]
struct CachedJobMetrics {
    snapshot: JobMetricsSnapshot,
    expires_at: Instant,
}

pub(super) async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    let job_metrics = load_job_metrics(&state).await;
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        state.metrics.render_prometheus_with_jobs(job_metrics),
    )
}

async fn load_job_metrics(state: &AppState) -> Option<JobMetricsSnapshot> {
    let cache = JOB_METRICS_CACHE.get_or_init(|| Mutex::new(None));
    let now = Instant::now();
    if let Ok(guard) = cache.lock() {
        if let Some(cached) = *guard {
            if cached.expires_at > now {
                return Some(cached.snapshot);
            }
        }
    }

    let snapshot = load_job_metrics_uncached(state).await?;
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedJobMetrics {
            snapshot,
            expires_at: Instant::now() + JOB_METRICS_CACHE_TTL,
        });
    }
    Some(snapshot)
}

async fn load_job_metrics_uncached(state: &AppState) -> Option<JobMetricsSnapshot> {
    let row = sqlx::query(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE status='queued') AS queued,
            COUNT(*) FILTER (WHERE status='running') AS running,
            COUNT(*) FILTER (WHERE status='failed') AS failed,
            COUNT(*) FILTER (WHERE status='succeeded') AS succeeded,
            COALESCE(
                EXTRACT(EPOCH FROM (NOW() - MIN(created_at) FILTER (WHERE status='queued'))),
                0
            )::double precision AS oldest_queued_age_seconds
        FROM video_jobs
        "#,
    )
    .fetch_one(&state.pool)
    .await
    .ok()?;

    Some(JobMetricsSnapshot {
        queued: row.try_get::<i64, _>("queued").ok()?.max(0) as u64,
        running: row.try_get::<i64, _>("running").ok()?.max(0) as u64,
        failed: row.try_get::<i64, _>("failed").ok()?.max(0) as u64,
        succeeded: row.try_get::<i64, _>("succeeded").ok()?.max(0) as u64,
        oldest_queued_age_seconds: row
            .try_get::<f64, _>("oldest_queued_age_seconds")
            .ok()
            .unwrap_or(0.0)
            .max(0.0) as u64,
    })
}

pub(super) async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let autonomi = match state.antd.health().await {
        Ok(status) => AutonomiHealth {
            ok: status.status.eq_ignore_ascii_case("ok"),
            network: status.network,
            error: None,
        },
        Err(err) => AutonomiHealth {
            ok: false,
            network: None,
            error: Some(err.to_string()),
        },
    };
    let postgres = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => PostgresHealth {
            ok: true,
            error: None,
        },
        Err(err) => PostgresHealth {
            ok: false,
            error: Some(err.to_string()),
        },
    };
    let write_ready = if state.config.antd_require_cost_ready {
        state
            .antd
            .data_cost_for_size(MIN_ANTD_SELF_ENCRYPTION_BYTES)
            .await
            .is_ok()
    } else {
        autonomi.ok
    };
    let ok = autonomi.ok && postgres.ok && write_ready;
    let status = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status,
        Json(HealthResponse {
            ok,
            autonomi,
            postgres,
            write_ready,
            payment_mode: state.config.antd_payment_mode.clone(),
            final_quote_approval_ttl_seconds: state.config.final_quote_approval_ttl_seconds,
            implementation: "rust_admin",
            role: "primary_admin",
        }),
    )
        .into_response()
}
